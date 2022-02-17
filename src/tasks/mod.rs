mod backend;
mod device;
mod output;
mod slow_clients;

use crate::tasks::backend::BackendEventHandler;
use crate::tasks::slow_clients::SlowClientHandler;
use crate::State;
use std::rc::Rc;

pub async fn handle_backend_events(state: Rc<State>) {
    let mut beh = BackendEventHandler { state };
    beh.handle_events().await;
}

pub async fn handle_slow_clients(state: Rc<State>) {
    let mut sch = SlowClientHandler { state };
    sch.handle_events().await;
}

pub async fn do_layout(state: Rc<State>) {
    loop {
        let node = state.pending_layout.pop().await;
        if node.needs_layout() {
            node.do_layout();
        }
    }
}
