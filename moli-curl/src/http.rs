//! HTTP submission types and owner-local request scheduling.
//! The shared runtime drives libcurl; this module owns HTTP jobs until completion.

pub(crate) mod registry;
mod scheduling;

use crate::{
    CurlDnsResolution, CurlTransferId,
    runtime::{CurlRuntimeCommand, identity::next_transfer_id},
};
use anyhow::{Result, anyhow};
use crossbeam_channel::Sender;
use curl::{
    easy::{Easy2, Handler},
    multi::MultiWaker,
};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// Origin key used by the curl scheduler for per-origin active transfer caps.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CurlOriginKey {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
}

impl CurlOriginKey {
    /// libcurl applies `CURLMOPT_MAX_HOST_CONNECTIONS` per host name and port,
    /// regardless of scheme, so `http` and `ws` transfers share one budget.
    pub(crate) fn connection_destination(&self) -> Option<CurlConnectionDestination> {
        Some(CurlConnectionDestination {
            host: self.host.clone(),
            port: self.port?,
        })
    }
}

/// The host name and port that libcurl's per-host connection cap counts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CurlConnectionDestination {
    pub(crate) host: String,
    pub(crate) port: u16,
}

impl CurlConnectionDestination {
    pub(crate) fn from_url(url: &url::Url) -> Option<Self> {
        Some(Self {
            host: url.host_str()?.to_owned(),
            port: url.port_or_known_default()?,
        })
    }
}

/// Caller-owned cancellation probe for a job that has not started yet.
pub type CurlJobCancelled = Arc<dyn Fn() -> bool + Send + Sync>;

/// How a transfer occupies libcurl's per-host connection cap
/// (`CURLMOPT_MAX_HOST_CONNECTIONS`) for its destination.
///
/// libcurl parks a transfer that finds the cap exhausted in its internal
/// pending queue, where `CURLOPT_CONNECTTIMEOUT` keeps running. The scheduler
/// therefore starts an [`Exclusive`](Self::Exclusive) transfer only when its
/// destination has a free connection, so its connect timeout bounds the real
/// TCP/TLS connect.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CurlConnectionUse {
    /// Neither counted nor gated, as for a transfer through a proxy: libcurl
    /// then counts the proxy host, not the origin.
    #[default]
    Untracked,
    /// A direct transfer that may share an HTTP/2 connection. It is counted
    /// against its destination but not gated, because a multiplexed stream
    /// does not need a free connection.
    Shared,
    /// A direct transfer that needs one whole connection for its lifetime,
    /// as with HTTP/1.x. It is counted and starts only below the cap.
    Exclusive,
}

/// A configured curl transfer plus scheduler metadata.
pub struct CurlMultiJob<H: Handler, C> {
    pub easy: Easy2<H>,
    pub context: C,
    pub origin: Option<CurlOriginKey>,
    /// Absolute deadline for the whole scheduler-owned transfer attempt.
    ///
    /// libcurl cannot account for time spent in Moli's priority queue or in
    /// the shared DNS residence because both happen before the easy handle is
    /// added to the multi handle. The owner enforces this deadline in those
    /// residences and gives libcurl only the remaining duration.
    pub deadline: Option<Instant>,
    /// DNS ownership chosen by the caller before this transfer enters curl.
    ///
    /// A no-shared-resolution policy leaves the already configured curl handle
    /// in charge of routing; it may need no DNS lookup at all. A shared-endpoint
    /// policy parks the transfer outside the curl multi handle set until the
    /// bounded system resolver publishes an answer.
    pub dns_resolution: CurlDnsResolution,
    /// Higher values start before lower values when jobs are queued.
    pub priority: u8,
    pub label: String,
    /// How this transfer uses libcurl's per-host connection cap.
    pub connection: CurlConnectionUse,
    /// Reports caller cancellation while the job waits in the scheduler, so
    /// a cancelled job is completed there instead of being started.
    pub cancelled: Option<CurlJobCancelled>,
}

impl<H: Handler, C> fmt::Debug for CurlMultiJob<H, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CurlMultiJob")
            .field("origin", &self.origin)
            .field("deadline", &self.deadline)
            .field("dns_resolution", &self.dns_resolution)
            .field("priority", &self.priority)
            .field("label", &self.label)
            .field("connection", &self.connection)
            .field("cancellable", &self.cancelled.is_some())
            .finish_non_exhaustive()
    }
}

/// Completion emitted by `CurlMultiRuntime`.
pub struct CurlMultiCompletion<H: Handler, C> {
    pub transfer_id: CurlTransferId,
    pub easy: Option<Easy2<H>>,
    pub context: C,
    pub result: Result<()>,
}

impl<H: Handler, C> fmt::Debug for CurlMultiCompletion<H, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CurlMultiCompletion")
            .field("transfer_id", &self.transfer_id)
            .field("has_easy", &self.easy.is_some())
            .field("result", &self.result.as_ref().map(|_| ()))
            .finish_non_exhaustive()
    }
}

/// Error returned when a job cannot be submitted and is returned to the caller.
pub struct CurlSubmitError<H: Handler, C> {
    pub job: CurlMultiJob<H, C>,
    pub error: anyhow::Error,
}

impl<H: Handler, C> fmt::Debug for CurlSubmitError<H, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CurlSubmitError")
            .field("job", &self.job)
            .field("error", &self.error)
            .finish()
    }
}

/// Submits HTTP work to one owner. Clones carry no shutdown or join authority.
#[derive(Debug)]
pub struct CurlHttpSender<H: Handler + Send + 'static, C: Send + 'static> {
    pub(crate) command_tx: Sender<CurlRuntimeCommand<H, C>>,
    pub(crate) owner_waker: MultiWaker,
    pub(crate) shutdown_requested: Arc<AtomicBool>,
}

impl<H: Handler + Send + 'static, C: Send + 'static> Clone for CurlHttpSender<H, C> {
    fn clone(&self) -> Self {
        Self {
            command_tx: self.command_tx.clone(),
            owner_waker: self.owner_waker.clone(),
            shutdown_requested: self.shutdown_requested.clone(),
        }
    }
}

impl<H: Handler + Send + 'static, C: Send + 'static> CurlHttpSender<H, C> {
    /// A rejected job comes back boxed: the job is large and submission
    /// fails only while the runtime shuts down.
    pub fn submit(
        &self,
        job: CurlMultiJob<H, C>,
    ) -> std::result::Result<CurlTransferId, Box<CurlSubmitError<H, C>>> {
        if self.shutdown_requested.load(Ordering::SeqCst) {
            return Err(Box::new(CurlSubmitError {
                job,
                error: anyhow!("curl multi runtime is shutting down"),
            }));
        }
        let transfer_id = match next_transfer_id() {
            Ok(transfer_id) => transfer_id,
            Err(error) => return Err(Box::new(CurlSubmitError { job, error })),
        };
        match self
            .command_tx
            .send(CurlRuntimeCommand::Request { transfer_id, job })
        {
            Ok(()) => {
                let _ = self.owner_waker.wakeup();
                Ok(transfer_id)
            }
            Err(error) => {
                let CurlRuntimeCommand::Request { job, .. } = error.into_inner() else {
                    unreachable!("submit only sends request commands");
                };
                Err(Box::new(CurlSubmitError {
                    job,
                    error: anyhow!("curl multi runtime is shutting down"),
                }))
            }
        }
    }
}
