use super::*;

const URL: &str = "https://web-locks.test/app/";

fn page() -> crate::runtime::PageVmTaskExecutorTestHarness {
    new_storage_page_task_executor_test_vm(URL)
}

#[test]
fn web_locks_grant_exclusive_requests_in_fifo_order() {
    let mut vm = page();
    vm.eval(
        r#"
        globalThis.log = [];
        navigator.locks.request("f", () => {
          log.push("1");
          return new Promise(resolve => globalThis.release = resolve);
        });
        navigator.locks.request("f", () => { log.push("2"); });
        navigator.locks.request("f", () => { log.push("3"); }).then(() => log.push("done"));
        "#,
    )
    .unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("log.join()").unwrap(),
        "1"
    );
    vm.eval("release()").unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("log.join()").unwrap(),
        "1,2,3,done"
    );
}

#[test]
fn web_locks_shared_holders_coexist_and_an_exclusive_request_waits_for_both() {
    let mut vm = page();
    vm.eval(
        r#"
        globalThis.log = [];
        globalThis.release = {};
        for (const key of ["a", "b"]) {
          navigator.locks.request("s", { mode: "shared" }, lock => {
            log.push(key + ":" + lock.mode);
            return new Promise(resolve => release[key] = resolve);
          });
        }
        navigator.locks.request("s", lock => { log.push("ex:" + lock.name + ":" + lock.mode); });
        "#,
    )
    .unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("log.join()").unwrap(),
        "a:shared,b:shared"
    );
    vm.eval("release.a()").unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("log.join()").unwrap(),
        "a:shared,b:shared"
    );
    vm.eval("release.b()").unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("log.join()").unwrap(),
        "a:shared,b:shared,ex:s:exclusive"
    );
}

#[test]
fn web_locks_if_available_passes_null_while_the_lock_is_held() {
    let mut vm = page();
    vm.eval(
        r#"
        globalThis.result = "pending";
        navigator.locks.request("i", () => new Promise(() => {}));
        navigator.locks
          .request("i", { ifAvailable: true }, lock => lock === null ? "null" : "lock")
          .then(value => result = value);
        "#,
    )
    .unwrap();
    assert_eq!(vm.eval_after_selected_page_tasks("result").unwrap(), "null");
}

#[test]
fn web_locks_steal_rejects_the_previous_holder_with_abort_error() {
    let mut vm = page();
    vm.eval(
        r#"
        globalThis.results = [];
        navigator.locks
          .request("t", () => new Promise(() => {}))
          .then(() => results.push("old resolved"), error => results.push("old " + error.name));
        "#,
    )
    .unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("results.join()").unwrap(),
        ""
    );
    vm.eval(
        r#"
        navigator.locks
          .request("t", { steal: true }, lock => "stole " + lock.name)
          .then(value => results.push(value));
        "#,
    )
    .unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("results.join()").unwrap(),
        "old AbortError,stole t"
    );
}

#[test]
fn web_locks_steal_rejects_a_settling_holder_and_retires_a_never_settling_one() {
    let mut vm = page();
    // The holder steals its own lock and settles in the same task, before the
    // steal task runs: the steal still wins.
    vm.eval(
        r#"
        globalThis.results = [];
        navigator.locks
          .request("race", () => {
            navigator.locks.request("race", { steal: true }, () => { results.push("thief"); });
            return 1;
          })
          .then(value => results.push("holder resolved " + value), error => results.push("holder " + error.name));
        navigator.locks
          .request("idle", () => new Promise(() => {}))
          .catch(error => results.push("idle " + error.name));
        "#,
    )
    .unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("results.join()").unwrap(),
        "holder AbortError,thief"
    );
    // A stolen holder whose callback never settles leaves no request behind.
    vm.eval(r#"navigator.locks.request("idle", { steal: true }, () => { results.push("idle thief"); });"#)
        .unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("results.join()").unwrap(),
        "holder AbortError,thief,idle AbortError,idle thief"
    );
    assert_eq!(vm._context_host.borrow().web_lock_request_count(), 0);
}

#[test]
fn web_locks_abort_in_the_requesting_task_skips_the_callback_and_frees_the_lock() {
    let mut vm = page();
    vm.eval(
        r#"
        globalThis.results = [];
        const controller = new AbortController();
        navigator.locks
          .request("sa", { signal: controller.signal }, () => { results.push("callback"); })
          .then(() => results.push("resolved"), error => results.push(error.name));
        controller.abort();
        const custom = new AbortController();
        navigator.locks
          .request("sr", { signal: custom.signal }, () => { results.push("callback"); })
          .then(() => results.push("resolved"), reason => results.push("reason:" + reason));
        custom.abort("x");
        "#,
    )
    .unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("results.join()").unwrap(),
        "AbortError,reason:x"
    );
    // Chromium releases the withdrawn lock asynchronously, so check after a
    // task turn rather than in the aborting task.
    vm.eval(
        r#"
        navigator.locks
          .request("sa", { ifAvailable: true }, lock => lock === null ? "held" : "free")
          .then(value => results.push(value));
        "#,
    )
    .unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("results.join()").unwrap(),
        "AbortError,reason:x,free"
    );
}

#[test]
fn web_locks_query_reports_held_and_pending_requests() {
    let mut vm = page();
    vm.eval(
        r#"
        globalThis.result = "pending";
        navigator.locks.request("q", () => new Promise(() => {}));
        navigator.locks.request("q", { mode: "shared" }, () => {});
        navigator.locks.query().then(({ held, pending }) => {
          result = JSON.stringify({
            held: held.map(lock => [lock.name, lock.mode]),
            pending: pending.map(lock => [lock.name, lock.mode]),
            client: typeof held[0].clientId === "string" && held[0].clientId === pending[0].clientId,
          });
        });
        "#,
    )
    .unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("result").unwrap(),
        r#"{"held":[["q","exclusive"]],"pending":[["q","shared"]],"client":true}"#
    );
}

#[test]
fn web_locks_reject_invalid_requests() {
    let mut vm = page();
    vm.eval(
        r#"
        globalThis.result = "pending";
        const name = promise => promise.then(() => "ok", error => error.name);
        Promise.all([
          name(navigator.locks.request("-x", () => {})),
          name(navigator.locks.request("v", { steal: true, ifAvailable: true }, () => {})),
          name(navigator.locks.request("v")),
          name(navigator.locks.request("v", { signal: AbortSignal.abort() }, () => {})),
        ]).then(names => result = names.join());
        "#,
    )
    .unwrap();
    assert_eq!(
        vm.eval_after_selected_page_tasks("result").unwrap(),
        "NotSupportedError,NotSupportedError,TypeError,AbortError"
    );
}

#[test]
fn web_locks_removing_an_iframe_releases_its_locks() {
    let mut vm = page();
    vm.eval(
        r#"
        globalThis.frame = document.createElement("iframe");
        (document.body || document.documentElement).appendChild(frame);
        frame.contentWindow.navigator.locks.request("rm", () => new Promise(() => {}));
        "#,
    )
    .unwrap();
    vm.eval(
        r#"
        globalThis.before = "pending";
        navigator.locks
          .request("rm", { ifAvailable: true }, lock => lock === null ? "held" : "free")
          .then(value => before = value);
        "#,
    )
    .unwrap();
    assert_eq!(vm.eval_after_selected_page_tasks("before").unwrap(), "held");
    vm.eval(
        r#"
        frame.remove();
        globalThis.after = "pending";
        navigator.locks
          .request("rm", { ifAvailable: true }, lock => lock === null ? "held" : "free")
          .then(value => after = value);
        "#,
    )
    .unwrap();
    assert_eq!(vm.eval_after_selected_page_tasks("after").unwrap(), "free");
}

#[test]
fn web_locks_are_shared_across_pages_and_released_when_a_page_closes() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default())
        .expect("Web Locks test loader");
    let owner = crate::runtime::RendererBrowserContextRuntime::new();
    let new_page = || {
        crate::runtime::PageVmTaskExecutorTestHarness::new_with_browser_context_runtime(
            url::Url::parse(URL).expect("Web Locks URL"),
            &loader,
            owner.handle(),
        )
    };
    let mut first = new_page();
    first
        .eval(
            r#"
            globalThis.holding = "pending";
            navigator.locks.request("cross", () => {
              holding = "holding";
              return new Promise(() => {});
            });
            "#,
        )
        .unwrap();
    assert_eq!(
        first.eval_after_selected_page_tasks("holding").unwrap(),
        "holding"
    );

    let mut second = new_page();
    second
        .eval(
            r#"
            globalThis.probe = "pending";
            globalThis.waiter = "pending";
            navigator.locks
              .request("cross", { ifAvailable: true }, lock => lock === null ? "held elsewhere" : "free")
              .then(value => probe = value);
            navigator.locks.request("cross", () => { waiter = "granted"; });
            "#,
        )
        .unwrap();
    assert_eq!(
        second
            .eval_after_selected_page_tasks("probe + '|' + waiter")
            .unwrap(),
        "held elsewhere|pending"
    );

    drop(first);
    assert_eq!(
        second.eval_after_selected_page_tasks("waiter").unwrap(),
        "granted"
    );
}
