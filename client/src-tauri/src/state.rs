use farder_node::node::PersonalNode;
use std::sync::Mutex;

pub struct AppState {
    pub node: Mutex<Option<PersonalNode>>,
}
impl AppState {
    pub fn new() -> Self { Self { node: Mutex::new(None) } }
}
