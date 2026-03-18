use std::collections::HashMap;

pub struct QueueSnapshot {
    pub model_queue: HashMap<String, usize>,
    pub node_queue: HashMap<String, usize>,
}
