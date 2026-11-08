//! HTTP jobs move through the priority queue, DNS and active easy handles.
//! The runtime alone calls perform/messages/poll and dispatches completions here.

use super::{
    CurlMultiCompletion, CurlMultiJob,
    scheduling::{
        CurlActiveTransfer, CurlPendingJob, active_origin_count, enqueue_existing_pending_job,
        enqueue_pending_job, exclusive_connection_slot_available, job_is_eligible,
        pending_origin_count, take_cancelled_pending_jobs, take_expired_pending_jobs,
        take_transfers_in_notification_order,
    },
};
use crate::{
    CurlConnectionUse, CurlMultiRuntimeConfig, CurlOriginKey, CurlTransferId,
    dns_adapter::{CurlDnsOwnerCompletion, CurlDnsOwnerResidence, CurlDnsReady},
    http::CurlConnectionDestination,
};
use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender};
use curl::{
    easy::Handler,
    multi::{Message, Multi},
};
use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroUsize,
    sync::OnceLock,
    time::{Duration, Instant},
};

pub(crate) struct HttpRegistry<H: Handler, C> {
    config: CurlMultiRuntimeConfig,
    completion_tx: Sender<CurlMultiCompletion<H, C>>,
    closed: bool,
    pending: VecDeque<CurlPendingJob<H, C>>,
    dns: CurlDnsOwnerResidence<CurlTransferId, CurlPendingJob<H, C>>,
    active: HashMap<CurlTransferId, CurlActiveTransfer<H, C>>,
}

impl<H: Handler, C> HttpRegistry<H, C> {
    pub(crate) fn new(
        config: CurlMultiRuntimeConfig,
        completion_tx: Sender<CurlMultiCompletion<H, C>>,
    ) -> Self {
        Self {
            config,
            completion_tx,
            closed: false,
            pending: VecDeque::new(),
            dns: CurlDnsOwnerResidence::default(),
            active: HashMap::new(),
        }
    }

    pub(crate) fn has_active(&self) -> bool {
        !self.active.is_empty()
    }

    /// DNS-only work can wait on its completion channel without polling curl.
    pub(crate) fn has_curl_work(&self) -> bool {
        self.has_active() || !self.pending.is_empty()
    }

    /// Pending work can run after completions release scheduler capacity.
    /// Use the same global and per-origin limits as the actual startup path.
    /// `other_connections` counts non-HTTP connections, such as WebSockets,
    /// that hold a libcurl per-host connection slot on the shared Multi.
    pub(crate) fn has_eligible_jobs(
        &self,
        other_connections: &dyn Fn(&CurlConnectionDestination) -> usize,
    ) -> bool {
        self.next_eligible_job_index(other_connections).is_some()
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .iter()
            .filter_map(|pending| pending.job.deadline)
            .chain(self.dns.next_deadline(|pending| pending.job.deadline))
            .min()
    }

    pub(crate) fn dns_completions(&self) -> &Receiver<CurlDnsOwnerCompletion<CurlTransferId>> {
        self.dns.completion_receiver()
    }

    pub(crate) fn advance(
        &mut self,
        multi: &mut Multi,
        other_connections: &dyn Fn(&CurlConnectionDestination) -> usize,
    ) {
        self.drain_dns_completions();
        self.expire_waiting_jobs();
        self.start_eligible_jobs(multi, other_connections);
    }

    pub(crate) fn contains(&self, id: CurlTransferId) -> bool {
        self.active.contains_key(&id)
    }

    pub(crate) fn completion_result(
        &self,
        id: CurlTransferId,
        message: &Message<'_>,
    ) -> Option<Result<(), curl::Error>> {
        message.result_for2(&self.active.get(&id)?.handle)
    }

    pub(crate) fn complete(
        &mut self,
        multi: &mut Multi,
        completed: Vec<(CurlTransferId, Result<(), curl::Error>)>,
    ) {
        for (transfer_id, active, result) in
            take_transfers_in_notification_order(&mut self.active, completed)
        {
            self.finish_active_transfer(multi, transfer_id, active, result.map_err(Into::into));
        }
    }

    pub(crate) fn admit(&mut self, transfer_id: CurlTransferId, job: CurlMultiJob<H, C>) {
        if self.closed {
            self.send_completion(CurlMultiCompletion {
                transfer_id,
                easy: Some(job.easy),
                context: job.context,
                result: Err(anyhow!("curl multi runtime is shutting down")),
            });
            return;
        }
        if curl_runtime_trace_enabled() {
            let origin = job.origin.as_ref();
            tracing::info!(
                target: "moli_cdp_nav_timing",
                transfer_id = %transfer_id,
                label = %job.label,
                origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
                origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
                origin_port = ?origin.and_then(|origin| origin.port),
                origin = ?job.origin,
                priority = job.priority,
                pending_before = self.pending.len(),
                pending_same_origin_before = origin
                    .map(|origin| pending_origin_count(&self.pending, origin))
                    .unwrap_or(0),
                stage = "curl_runtime_job_queued",
            );
        }
        enqueue_pending_job(&mut self.pending, transfer_id, job);
    }

    pub(crate) fn shutdown(&mut self, multi: &mut Multi) {
        if self.closed {
            return;
        }
        self.closed = true;
        while let Some(pending) = self.pending.pop_front() {
            let CurlPendingJob {
                transfer_id, job, ..
            } = pending;
            self.send_completion(CurlMultiCompletion {
                transfer_id,
                easy: Some(job.easy),
                context: job.context,
                result: Err(anyhow!("curl multi runtime is shutting down")),
            });
        }
        for pending in self.dns.drain() {
            let CurlPendingJob {
                transfer_id, job, ..
            } = pending;
            let _ = self.completion_tx.send(CurlMultiCompletion {
                transfer_id,
                easy: Some(job.easy),
                context: job.context,
                result: Err(anyhow!(
                    "curl multi runtime DNS request cancelled during shutdown"
                )),
            });
        }
        for (transfer_id, active) in self.active.drain() {
            let easy = multi.remove2(active.handle).ok();
            let _ = self.completion_tx.send(CurlMultiCompletion {
                transfer_id,
                easy,
                context: active.context,
                result: Err(anyhow!(
                    "curl multi runtime request cancelled during shutdown"
                )),
            });
        }
    }

    fn next_eligible_job_index(
        &self,
        other_connections: &dyn Fn(&CurlConnectionDestination) -> usize,
    ) -> Option<usize> {
        if self.closed || self.active.len() >= self.config.max_active.get() {
            return None;
        }
        // Destinations where an older exclusive job is still waiting. Newer
        // shareable jobs there would keep every slot busy and could hold the
        // exclusive job until its deadline, so they wait behind it.
        let mut held_destinations = Vec::new();
        for (index, pending) in self.pending.iter().enumerate() {
            let origin = pending.job.origin.as_ref();
            let connection = pending.job.connection;
            if connection == CurlConnectionUse::Shared
                && !held_destinations.is_empty()
                && origin
                    .and_then(CurlOriginKey::connection_destination)
                    .is_some_and(|destination| held_destinations.contains(&destination))
            {
                continue;
            }
            if job_is_eligible(origin, &self.active, self.config.max_host_active)
                && exclusive_connection_slot_available(
                    origin,
                    connection,
                    &self.active,
                    self.config.max_host_connections,
                    other_connections,
                )
            {
                return Some(index);
            }
            if connection == CurlConnectionUse::Exclusive
                && let Some(destination) = origin.and_then(CurlOriginKey::connection_destination)
            {
                held_destinations.push(destination);
            }
        }
        None
    }

    fn start_eligible_jobs(
        &mut self,
        multi: &mut Multi,
        other_connections: &dyn Fn(&CurlConnectionDestination) -> usize,
    ) {
        while let Some(index) = self.next_eligible_job_index(other_connections) {
            let pending = self
                .pending
                .remove(index)
                .expect("pending curl job index should exist");
            let dns_target = pending.job.dns_resolution.target().cloned();
            match dns_target {
                Some(target) => self
                    .dns
                    .start(pending.transfer_id, pending, target, multi.waker()),
                None => self.start_job(multi, pending),
            }
        }
    }

    fn drain_dns_completions(&mut self) {
        while let Some(ready) = self.dns.try_claim_next() {
            self.handle_dns_completion(ready);
        }
    }

    pub(crate) fn claim_dns_completion(
        &mut self,
        completion: CurlDnsOwnerCompletion<CurlTransferId>,
    ) {
        let Some(ready) = self.dns.claim(completion) else {
            return;
        };
        self.handle_dns_completion(ready);
    }

    fn handle_dns_completion(&mut self, ready: CurlDnsReady<CurlPendingJob<H, C>>) {
        let mut pending = ready.pending;
        if self.closed {
            let CurlPendingJob {
                transfer_id, job, ..
            } = pending;
            self.send_completion(CurlMultiCompletion {
                transfer_id,
                easy: Some(job.easy),
                context: job.context,
                result: Err(anyhow!("curl multi runtime is shutting down")),
            });
            return;
        }
        if pending.deadline_reached(Instant::now()) {
            self.complete_timed_out_job(pending, "while waiting for DNS");
            return;
        }
        match ready.result {
            Ok(addresses) => {
                if let Err(error) = pending
                    .job
                    .dns_resolution
                    .install(&mut pending.job.easy, addresses.as_ref())
                {
                    let CurlPendingJob {
                        transfer_id, job, ..
                    } = pending;
                    self.send_completion(CurlMultiCompletion {
                        transfer_id,
                        easy: Some(job.easy),
                        context: job.context,
                        result: Err(error),
                    });
                    return;
                }
                enqueue_existing_pending_job(&mut self.pending, pending);
            }
            Err(error) => {
                // DNS failure is terminal while the easy handle still resides
                // outside the Multi. Do not requeue it: that would let curl
                // resolve or connect independently of the failed shared lookup.
                let CurlPendingJob {
                    transfer_id, job, ..
                } = pending;
                self.send_completion(CurlMultiCompletion {
                    transfer_id,
                    easy: Some(job.easy),
                    context: job.context,
                    result: Err(anyhow!(error.to_string())),
                });
            }
        }
    }

    fn start_job(&mut self, multi: &mut Multi, pending: CurlPendingJob<H, C>) {
        if pending.deadline_reached(Instant::now()) {
            self.complete_timed_out_job(pending, "while waiting to start");
            return;
        }
        if pending.cancelled() {
            self.complete_cancelled_job(pending);
            return;
        }
        let transfer_id = pending.transfer_id;
        let queued_for = pending.enqueued_at.elapsed();
        let mut job = pending.job;
        let label = job.label.clone();
        if let Some(deadline) = job.deadline {
            let Some(remaining) = curl_timeout_for_deadline(deadline, Instant::now()) else {
                self.send_completion(CurlMultiCompletion {
                    transfer_id,
                    easy: Some(job.easy),
                    context: job.context,
                    result: Err(curl_runtime_timeout_error("while waiting to start")),
                });
                return;
            };
            if let Err(error) = job.easy.timeout(remaining) {
                self.send_completion(CurlMultiCompletion {
                    transfer_id,
                    easy: Some(job.easy),
                    context: job.context,
                    result: Err(error).context("failed to apply remaining curl request deadline"),
                });
                return;
            }
        }
        match multi
            .add2(job.easy)
            .with_context(|| anyhow!("failed to add curl easy handle for {label}"))
        {
            Ok(mut handle) => {
                handle
                    .set_token(transfer_id.token())
                    .expect("active curl handle must accept its transfer token");
                if curl_runtime_trace_enabled() {
                    let origin = job.origin.as_ref();
                    tracing::info!(
                        target: "moli_cdp_nav_timing",
                        transfer_id = %transfer_id,
                        label = %label,
                        origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
                        origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
                        origin_port = ?origin.and_then(|origin| origin.port),
                        origin = ?job.origin,
                        priority = job.priority,
                        queued_ms = queued_for.as_millis(),
                        active_before = self.active.len(),
                        active_same_origin_before = origin
                            .map(|origin| active_origin_count(&self.active, origin))
                            .unwrap_or(0),
                        pending_after = self.pending.len(),
                        pending_same_origin_after = origin
                            .map(|origin| pending_origin_count(&self.pending, origin))
                            .unwrap_or(0),
                        max_active = self.config.max_active.get(),
                        max_host_active = ?self.config.max_host_active.map(NonZeroUsize::get),
                        max_host_connections = ?self.config.max_host_connections.map(NonZeroUsize::get),
                        max_total_connections = ?self.config.max_total_connections.map(NonZeroUsize::get),
                        max_concurrent_streams = ?self.config.max_concurrent_streams.map(NonZeroUsize::get),
                        multiplex = self.config.multiplex,
                        stage = "curl_runtime_job_start",
                    );
                }
                let previous = self.active.insert(
                    transfer_id,
                    CurlActiveTransfer {
                        handle,
                        context: job.context,
                        origin: job.origin,
                        priority: job.priority,
                        label,
                        started_at: Instant::now(),
                        queued_for,
                        connection: job.connection,
                    },
                );
                assert!(previous.is_none(), "curl transfer identity is unique");
            }
            Err(error) => self.send_completion(CurlMultiCompletion {
                transfer_id,
                easy: None,
                context: job.context,
                result: Err(error),
            }),
        }
    }

    fn expire_waiting_jobs(&mut self) {
        let now = Instant::now();
        for pending in take_expired_pending_jobs(&mut self.pending, now) {
            self.complete_timed_out_job(pending, "while waiting in the scheduler");
        }
        for pending in self.dns.take_expired(now, |pending| pending.job.deadline) {
            self.complete_timed_out_job(pending, "while waiting for DNS");
        }
        // A burst of queued work can be abandoned at once, as when a page
        // navigates away. Complete it here rather than starting it in curl.
        for pending in take_cancelled_pending_jobs(&mut self.pending) {
            self.complete_cancelled_job(pending);
        }
    }

    fn complete_cancelled_job(&self, pending: CurlPendingJob<H, C>) {
        let CurlPendingJob {
            transfer_id, job, ..
        } = pending;
        self.send_completion(CurlMultiCompletion {
            transfer_id,
            easy: Some(job.easy),
            context: job.context,
            result: Err(anyhow!(
                "curl multi runtime request cancelled while waiting to start"
            )),
        });
    }

    fn complete_timed_out_job(&self, pending: CurlPendingJob<H, C>, stage: &'static str) {
        let CurlPendingJob {
            transfer_id, job, ..
        } = pending;
        self.send_completion(CurlMultiCompletion {
            transfer_id,
            easy: Some(job.easy),
            context: job.context,
            result: Err(curl_runtime_timeout_error(stage)),
        });
    }

    fn finish_active_transfer(
        &self,
        multi: &mut Multi,
        transfer_id: CurlTransferId,
        active: CurlActiveTransfer<H, C>,
        result: Result<()>,
    ) {
        let easy = match multi.remove2(active.handle) {
            Ok(easy) => Some(easy),
            Err(error) => {
                self.send_completion(CurlMultiCompletion {
                    transfer_id,
                    easy: None,
                    context: active.context,
                    result: Err(anyhow!(
                        "failed to remove curl easy handle for {}: {error}",
                        active.label
                    )),
                });
                return;
            }
        };
        if curl_runtime_trace_enabled() {
            let origin = active.origin.as_ref();
            match &result {
                Ok(()) => {
                    tracing::info!(
                        target: "moli_cdp_nav_timing",
                        transfer_id = %transfer_id,
                        label = %active.label,
                        origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
                        origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
                        origin_port = ?origin.and_then(|origin| origin.port),
                        origin = ?active.origin,
                        priority = active.priority,
                        ok = true,
                        active_ms = active.started_at.elapsed().as_millis(),
                        queued_ms = active.queued_for.as_millis(),
                        active_remaining = self.active.len(),
                        active_same_origin_remaining = origin
                            .map(|origin| active_origin_count(&self.active, origin))
                            .unwrap_or(0),
                        pending_after = self.pending.len(),
                        pending_same_origin_after = origin
                            .map(|origin| pending_origin_count(&self.pending, origin))
                            .unwrap_or(0),
                        stage = "curl_runtime_job_done",
                    );
                }
                Err(error) => {
                    tracing::info!(
                        target: "moli_cdp_nav_timing",
                        transfer_id = %transfer_id,
                        label = %active.label,
                        origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
                        origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
                        origin_port = ?origin.and_then(|origin| origin.port),
                        origin = ?active.origin,
                        priority = active.priority,
                        ok = false,
                        error = %error,
                        active_ms = active.started_at.elapsed().as_millis(),
                        queued_ms = active.queued_for.as_millis(),
                        active_remaining = self.active.len(),
                        active_same_origin_remaining = origin
                            .map(|origin| active_origin_count(&self.active, origin))
                            .unwrap_or(0),
                        pending_after = self.pending.len(),
                        pending_same_origin_after = origin
                            .map(|origin| pending_origin_count(&self.pending, origin))
                            .unwrap_or(0),
                        stage = "curl_runtime_job_done",
                    );
                }
            }
        }
        let result = result.with_context(|| {
            anyhow!(
                "curl request failed for {} after active={}ms queued={}ms",
                active.label,
                active.started_at.elapsed().as_millis(),
                active.queued_for.as_millis()
            )
        });
        self.send_completion(CurlMultiCompletion {
            transfer_id,
            easy,
            context: active.context,
            result,
        });
    }

    fn send_completion(&self, completion: CurlMultiCompletion<H, C>) {
        let _ = self.completion_tx.send(completion);
    }
}

fn curl_runtime_timeout_error(stage: &str) -> anyhow::Error {
    anyhow!("curl multi runtime request timed out {stage}")
}

fn curl_timeout_for_deadline(deadline: Instant, now: Instant) -> Option<Duration> {
    let remaining = deadline.saturating_duration_since(now);
    // curl-rust converts CURLOPT_TIMEOUT_MS with `Duration::as_millis()`. A
    // positive sub-millisecond value would therefore become zero, which
    // libcurl interprets as disabling the timeout entirely.
    (remaining >= Duration::from_millis(1)).then_some(remaining)
}

fn curl_runtime_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        env_flag_enabled("MOLI_CDP_NAV_TIMING") || env_flag_enabled("MOLI_CURL_RUNTIME_TRACE")
    })
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CurlDnsResolution;
    use curl::easy::Easy2;
    use std::{
        cell::Cell,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    #[derive(Debug)]
    struct TestHandler;

    impl Handler for TestHandler {}

    fn test_job(
        label: &str,
        priority: u8,
        origin: Option<CurlOriginKey>,
    ) -> CurlMultiJob<TestHandler, String> {
        CurlMultiJob {
            easy: Easy2::new(TestHandler),
            context: label.to_owned(),
            origin,
            deadline: None,
            dns_resolution: CurlDnsResolution::no_shared_resolution(),
            priority,
            label: label.to_owned(),
            connection: CurlConnectionUse::Untracked,
            cancelled: None,
        }
    }

    fn no_other_connections(_: &CurlConnectionDestination) -> usize {
        0
    }

    fn test_transfer_id(sequence: usize) -> CurlTransferId {
        CurlTransferId::from_token(sequence).expect("test transfer ID is non-zero")
    }

    #[test]
    fn eligible_jobs_respect_global_and_origin_capacity_after_completions() {
        let config = CurlMultiRuntimeConfig {
            max_active: NonZeroUsize::new(2).unwrap(),
            max_host_active: NonZeroUsize::new(1),
            ..CurlMultiRuntimeConfig::default()
        };
        let (completion_tx, _completed) = crossbeam_channel::unbounded();
        let mut multi = Multi::new();
        let mut registry = HttpRegistry::new(config, completion_tx);
        let origin = |host: &str| CurlOriginKey {
            scheme: "https".to_owned(),
            host: host.to_owned(),
            port: Some(443),
        };
        let first = test_transfer_id(1);
        let blocked = test_transfer_id(2);
        let other_origin = test_transfer_id(3);
        let no_origin = test_transfer_id(4);

        registry.admit(first, test_job("first", 1, Some(origin("a.test"))));
        registry.advance(&mut multi, &no_other_connections);
        assert!(registry.contains(first));
        assert!(!registry.has_eligible_jobs(&no_other_connections));

        // A free global slot does not bypass the origin cap.
        registry.admit(blocked, test_job("blocked", 2, Some(origin("a.test"))));
        assert!(!registry.has_eligible_jobs(&no_other_connections));
        // A blocked high-priority head must not hide eligible work elsewhere.
        registry.admit(other_origin, test_job("other", 0, Some(origin("b.test"))));
        assert!(registry.has_eligible_jobs(&no_other_connections));
        registry.advance(&mut multi, &no_other_connections);
        assert!(registry.contains(other_origin));
        assert!(!registry.contains(blocked));

        // The global cap also applies to jobs without an origin key.
        registry.admit(no_origin, test_job("no-origin", 0, None));
        assert!(!registry.has_eligible_jobs(&no_other_connections));
        registry.complete(&mut multi, vec![(other_origin, Ok(()))]);
        assert!(registry.has_eligible_jobs(&no_other_connections));
        registry.advance(&mut multi, &no_other_connections);
        assert!(registry.contains(no_origin));
        assert!(!registry.contains(blocked));

        registry.complete(&mut multi, vec![(first, Ok(()))]);
        assert!(registry.has_eligible_jobs(&no_other_connections));
        registry.advance(&mut multi, &no_other_connections);
        assert!(registry.contains(blocked));
        assert!(!registry.has_eligible_jobs(&no_other_connections));
        registry.shutdown(&mut multi);
        assert!(!registry.has_eligible_jobs(&no_other_connections));
    }

    fn origin_at(host: &str, port: u16) -> CurlOriginKey {
        CurlOriginKey {
            scheme: "http".to_owned(),
            host: host.to_owned(),
            port: Some(port),
        }
    }

    fn job_using(
        label: &str,
        origin: CurlOriginKey,
        connection: CurlConnectionUse,
    ) -> CurlMultiJob<TestHandler, String> {
        let mut job = test_job(label, 1, Some(origin));
        job.connection = connection;
        job
    }

    fn registry_with_host_cap(
        max_host_connections: usize,
    ) -> (
        HttpRegistry<TestHandler, String>,
        Multi,
        Receiver<CurlMultiCompletion<TestHandler, String>>,
    ) {
        let config = CurlMultiRuntimeConfig {
            max_host_connections: NonZeroUsize::new(max_host_connections),
            ..CurlMultiRuntimeConfig::default()
        };
        let (completion_tx, completed) = crossbeam_channel::unbounded();
        (
            HttpRegistry::new(config, completion_tx),
            Multi::new(),
            completed,
        )
    }

    #[test]
    fn exclusive_jobs_wait_until_a_failed_transfer_releases_its_slot() {
        let (mut registry, mut multi, _completed) = registry_with_host_cap(2);
        let host = origin_at("a.test", 80);
        let first = test_transfer_id(1);
        let second = test_transfer_id(2);
        let waiting = test_transfer_id(3);
        // A shareable transfer may have negotiated HTTP/1.1 and hold a whole
        // connection, so it occupies a slot too.
        registry.admit(
            first,
            job_using("first", host.clone(), CurlConnectionUse::Shared),
        );
        registry.admit(
            second,
            job_using("second", host.clone(), CurlConnectionUse::Exclusive),
        );
        registry.admit(
            waiting,
            job_using("waiting", host.clone(), CurlConnectionUse::Exclusive),
        );
        registry.advance(&mut multi, &no_other_connections);
        assert!(registry.contains(first) && registry.contains(second));
        assert!(!registry.contains(waiting));
        assert!(!registry.has_eligible_jobs(&no_other_connections));

        // Proxied work holds a slot on the proxy host, not on this origin.
        let proxied = test_transfer_id(5);
        registry.admit(
            proxied,
            job_using("proxied", host, CurlConnectionUse::Untracked),
        );
        registry.advance(&mut multi, &no_other_connections);
        assert!(registry.contains(proxied));
        assert!(!registry.contains(waiting));

        registry.complete(&mut multi, vec![(second, Err(curl::Error::new(28)))]);
        assert!(registry.has_eligible_jobs(&no_other_connections));
        registry.advance(&mut multi, &no_other_connections);
        assert!(registry.contains(waiting));
        registry.shutdown(&mut multi);
    }

    #[test]
    fn newer_shared_jobs_wait_behind_an_older_exclusive_job_to_the_same_host() {
        let (mut registry, mut multi, _completed) = registry_with_host_cap(2);
        let host = origin_at("x.test", 443);
        let streams = [test_transfer_id(1), test_transfer_id(2)];
        for (index, stream) in streams.into_iter().enumerate() {
            registry.admit(
                stream,
                job_using(
                    &format!("stream-{index}"),
                    host.clone(),
                    CurlConnectionUse::Shared,
                ),
            );
        }
        registry.advance(&mut multi, &no_other_connections);
        assert!(streams.iter().all(|stream| registry.contains(*stream)));

        // An HTTP/1 fallback waits for a slot; newer shareable work to the
        // same host waits behind it, while other hosts are unaffected.
        let fallback = test_transfer_id(3);
        let newer = test_transfer_id(4);
        let other_host = test_transfer_id(5);
        registry.admit(
            fallback,
            job_using("fallback", host.clone(), CurlConnectionUse::Exclusive),
        );
        registry.admit(newer, job_using("newer", host, CurlConnectionUse::Shared));
        registry.admit(
            other_host,
            job_using(
                "other-host",
                origin_at("y.test", 443),
                CurlConnectionUse::Shared,
            ),
        );
        registry.advance(&mut multi, &no_other_connections);
        assert!(registry.contains(other_host));
        assert!(!registry.contains(fallback));
        assert!(!registry.contains(newer));
        assert!(!registry.has_eligible_jobs(&no_other_connections));

        registry.complete(&mut multi, vec![(streams[0], Ok(()))]);
        assert_eq!(
            registry.next_eligible_job_index(&no_other_connections),
            registry
                .pending
                .iter()
                .position(|pending| pending.transfer_id == fallback),
            "the waiting fallback takes the freed slot first"
        );
        registry.advance(&mut multi, &no_other_connections);
        assert!(registry.contains(fallback));
        // Once the fallback holds its connection, shareable work resumes.
        assert!(registry.contains(newer));
        registry.shutdown(&mut multi);
    }

    #[test]
    fn exclusive_job_starts_when_a_same_port_websocket_closes() {
        let (mut registry, mut multi, _completed) = registry_with_host_cap(1);
        let websockets = Cell::new(1);
        let websocket_on_a_80 = |destination: &CurlConnectionDestination| {
            if destination.host == "a.test" && destination.port == 80 {
                websockets.get()
            } else {
                0
            }
        };
        let blocked = test_transfer_id(1);
        let other_port = test_transfer_id(2);
        let other_host = test_transfer_id(3);

        registry.admit(
            blocked,
            job_using(
                "blocked",
                origin_at("a.test", 80),
                CurlConnectionUse::Exclusive,
            ),
        );
        registry.admit(
            other_port,
            job_using(
                "other-port",
                origin_at("a.test", 8080),
                CurlConnectionUse::Exclusive,
            ),
        );
        registry.admit(
            other_host,
            job_using(
                "other-host",
                origin_at("b.test", 80),
                CurlConnectionUse::Exclusive,
            ),
        );
        registry.advance(&mut multi, &websocket_on_a_80);
        assert!(!registry.contains(blocked));
        assert!(registry.contains(other_port));
        assert!(registry.contains(other_host));
        assert!(!registry.has_eligible_jobs(&websocket_on_a_80));

        websockets.set(0);
        assert!(registry.has_eligible_jobs(&websocket_on_a_80));
        registry.advance(&mut multi, &websocket_on_a_80);
        assert!(registry.contains(blocked));
        registry.shutdown(&mut multi);
    }

    #[test]
    fn cancelled_queued_job_completes_without_starting() {
        let (mut registry, mut multi, completed) = registry_with_host_cap(1);
        let host = origin_at("a.test", 80);
        let holder = test_transfer_id(1);
        let queued = test_transfer_id(2);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut queued_job = job_using("queued", host.clone(), CurlConnectionUse::Exclusive);
        queued_job.cancelled = Some(Arc::new({
            let cancel = cancel.clone();
            move || cancel.load(Ordering::SeqCst)
        }));
        registry.admit(
            holder,
            job_using("holder", host, CurlConnectionUse::Exclusive),
        );
        registry.admit(queued, queued_job);
        registry.advance(&mut multi, &no_other_connections);
        assert!(!registry.contains(queued));

        cancel.store(true, Ordering::SeqCst);
        registry.advance(&mut multi, &no_other_connections);
        let completion = completed.try_recv().expect("cancelled job completes");
        assert_eq!(completion.transfer_id, queued);
        assert!(
            completion
                .result
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );

        // The released queue position must not start later.
        registry.complete(&mut multi, vec![(holder, Ok(()))]);
        registry.advance(&mut multi, &no_other_connections);
        assert!(!registry.contains(queued));
        assert!(!registry.has_curl_work());
    }

    #[test]
    fn active_transfer_wait_is_capped_by_the_earliest_queued_deadline() {
        let now = Instant::now();
        let later = now + Duration::from_secs(2);
        let earlier = now + Duration::from_secs(1);
        let mut later_job = test_job("later", 1, None);
        later_job.deadline = Some(later);
        let mut earlier_job = test_job("earlier", 1, None);
        earlier_job.deadline = Some(earlier);
        let mut pending = VecDeque::new();
        enqueue_pending_job(&mut pending, test_transfer_id(1), later_job);
        enqueue_pending_job(&mut pending, test_transfer_id(2), earlier_job);
        let (completion_tx, _) = crossbeam_channel::unbounded();
        let mut registry = HttpRegistry::new(CurlMultiRuntimeConfig::default(), completion_tx);
        registry.pending = pending;

        assert_eq!(registry.next_deadline(), Some(earlier));
    }

    #[test]
    fn sub_millisecond_deadline_never_disables_the_libcurl_timeout() {
        let now = Instant::now();
        assert_eq!(
            curl_timeout_for_deadline(now + Duration::from_micros(999), now),
            None
        );
        assert_eq!(
            curl_timeout_for_deadline(now + Duration::from_millis(1), now),
            Some(Duration::from_millis(1))
        );
    }
}
