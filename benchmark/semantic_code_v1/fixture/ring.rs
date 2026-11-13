use std::collections::VecDeque;

pub fn insert_latest(queue: &mut VecDeque<u64>, value: u64, capacity: usize) {
    if capacity == 0 {
        return;
    }
    if queue.len() == capacity {
        queue.pop_front();
    }
    queue.push_back(value);
}
