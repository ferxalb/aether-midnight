use std::path::{Component, Path};

pub fn confined(relative: &Path) -> bool {
    relative.components().all(|part| matches!(part, Component::Normal(_)))
}
