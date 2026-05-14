#![allow(clippy::unwrap_used, clippy::expect_used)]
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SG-0 smoke tests for `tritond-saga`.
//!
//! The trivial saga is two actions:
//! * `step_a`: returns `"a"` (an owned String); undo flips a flag.
//! * `step_b`: reads `"a"` via `lookup`, returns `"b"`; can be
//!   configured to fail to exercise unwind.
//!
//! Cases:
//! 1. Run to `Done` on `MemSecStore`; assert step_a's undo did *not* run.
//! 2. Force `step_b` to fail; assert step_a's undo *did* run.
//! 3. Build a fresh executor over the same `Arc<MemSecStore>`
//!    (simulating a `tritond` restart) and call
//!    `recover_all_for_sec`; with the saga terminal there's nothing
//!    to do (returns 0).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use slog::{Drain, o};
use tokio::sync::Mutex;
use tritond_saga::{
    ActionContext, ActionError, ActionFunc, ActionRegistry, MemSecStore, SagaDag, SagaExecutor,
    SagaId, SagaName, SecEpoch, SecId, TritondSagaType,
};

const SAGA_NAME: &str = "trivial-saga";
const SAGA_VERSION: u32 = 1;

#[derive(Default)]
struct UndoSpy {
    a_undone: AtomicBool,
    fail_b: AtomicBool,
}

// Process-global spy + serialised test execution. SG-0 keeps the
// action functions free-standing; real catalogs in SG-2 onwards
// thread their deps through SagaContext::user_data().
static SPY: std::sync::OnceLock<Arc<UndoSpy>> = std::sync::OnceLock::new();
static TEST_GUARD: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();

fn test_guard() -> &'static Mutex<()> {
    TEST_GUARD.get_or_init(|| Mutex::new(()))
}

fn reset_spy(fail_b: bool) -> Arc<UndoSpy> {
    let spy = SPY.get_or_init(|| Arc::new(UndoSpy::default())).clone();
    spy.a_undone.store(false, Ordering::SeqCst);
    spy.fail_b.store(fail_b, Ordering::SeqCst);
    spy
}

type SgCtx = ActionContext<TritondSagaType>;

async fn step_a(_ctx: SgCtx) -> Result<String, ActionError> {
    Ok("a".to_string())
}

async fn step_a_undo(_ctx: SgCtx) -> Result<(), anyhow::Error> {
    if let Some(spy) = SPY.get() {
        spy.a_undone.store(true, Ordering::SeqCst);
    }
    Ok(())
}

async fn step_b(ctx: SgCtx) -> Result<String, ActionError> {
    let _a: String = ctx.lookup("step_a")?;
    if SPY
        .get()
        .map(|s| s.fail_b.load(Ordering::SeqCst))
        .unwrap_or(false)
    {
        return Err(ActionError::action_failed(
            "forced failure for unwind test".to_string(),
        ));
    }
    Ok("b".to_string())
}

async fn step_b_undo(_ctx: SgCtx) -> Result<(), anyhow::Error> {
    Ok(())
}

fn null_logger() -> slog::Logger {
    let drain = slog::Discard;
    slog::Logger::root(drain.fuse(), o!())
}

fn build_dag_and_registry() -> (Arc<SagaDag>, ActionRegistry) {
    let step_a_action = ActionFunc::new_action("step_a", step_a, step_a_undo);
    let step_b_action = ActionFunc::new_action("step_b", step_b, step_b_undo);

    let mut reg = ActionRegistry::new();
    reg.register(step_a_action.clone());
    reg.register(step_b_action.clone());

    let mut b = steno::DagBuilder::new(SagaName::new(SAGA_NAME));
    b.append(steno::Node::action(
        "step_a",
        "step_a-label",
        &*step_a_action,
    ));
    b.append(steno::Node::action(
        "step_b",
        "step_b-label",
        &*step_b_action,
    ));
    let dag = b.build().unwrap_or_else(|e| panic!("dag build: {e}"));
    let saga_dag = Arc::new(SagaDag::new(dag, serde_json::Value::Null));
    (saga_dag, reg)
}

fn build_executor(store: Arc<MemSecStore>) -> (SagaExecutor, Arc<SagaDag>) {
    let (dag, reg) = build_dag_and_registry();
    let mut exec =
        SagaExecutor::new_for_test(SecId::random(), SecEpoch::new(1), store, reg, null_logger());
    exec.register_saga_version(SAGA_NAME, SAGA_VERSION);
    (exec, dag)
}

#[tokio::test]
async fn trivial_saga_runs_to_done() {
    let _guard = test_guard().lock().await;
    reset_spy(false);
    let store = MemSecStore::new();
    let (exec, dag) = build_executor(store.clone());
    let saga_id = SagaId(uuid::Uuid::new_v4());
    let result = exec
        .saga_execute(saga_id, SAGA_NAME, SAGA_VERSION, dag)
        .await
        .expect("saga_execute should not surface an engine error");
    let ok = match result.kind {
        Ok(ok) => ok,
        Err(e) => panic!("saga should succeed; got error: {:?}", e),
    };
    let final_output: String = ok
        .lookup_node_output("step_b")
        .expect("step_b output should be present");
    assert_eq!(final_output, "b");
    let spy = SPY.get().expect("spy initialised");
    assert!(
        !spy.a_undone.load(Ordering::SeqCst),
        "step_a undo must not run on the happy path"
    );
    let n = store.event_count(saga_id).await;
    assert!(n > 0, "node-event log must have entries; got {n}");
}

#[tokio::test]
async fn trivial_saga_unwinds_on_action_failure() {
    let _guard = test_guard().lock().await;
    reset_spy(true);
    let store = MemSecStore::new();
    let (exec, dag) = build_executor(store.clone());
    let saga_id = SagaId(uuid::Uuid::new_v4());
    let result = exec
        .saga_execute(saga_id, SAGA_NAME, SAGA_VERSION, dag)
        .await
        .expect("saga_execute should not surface an engine error");
    assert!(
        result.kind.is_err(),
        "saga should report failure when step_b errors"
    );
    let spy = SPY.get().expect("spy initialised");
    assert!(
        spy.a_undone.load(Ordering::SeqCst),
        "step_a undo must run after step_b fails"
    );
}

#[tokio::test]
async fn recover_terminal_saga_is_a_noop() {
    let _guard = test_guard().lock().await;
    reset_spy(false);
    let store = MemSecStore::new();
    {
        let (exec, dag) = build_executor(store.clone());
        let saga_id = SagaId(uuid::Uuid::new_v4());
        exec.saga_execute(saga_id, SAGA_NAME, SAGA_VERSION, dag)
            .await
            .expect("first run succeeds");
    }
    let (exec2, _dag) = build_executor(store.clone());
    let n = exec2
        .recover_all_for_sec()
        .await
        .expect("recover_all_for_sec should not error");
    assert_eq!(
        n, 0,
        "no non-Done sagas should remain after a happy-path run"
    );
}
