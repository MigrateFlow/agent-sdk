use std::collections::HashMap;

use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};

use crate::error::{SdkError, SdkResult, TaskId};
use crate::types::task::Task;

pub struct TaskGraph {
    graph: DiGraph<TaskId, ()>,
    node_map: HashMap<TaskId, NodeIndex>,
}

impl Default for TaskGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            node_map: HashMap::new(),
        }
    }

    pub fn from_tasks(tasks: &[Task]) -> SdkResult<Self> {
        let mut tg = Self::new();

        for task in tasks {
            tg.add_task(task.id);
        }

        for task in tasks {
            for dep_id in &task.dependencies {
                if !tg.node_map.contains_key(dep_id) {
                    return Err(SdkError::TaskNotFound { task_id: *dep_id });
                }
                tg.add_dependency(task.id, *dep_id)?;
            }
        }

        tg.check_cycles()?;
        Ok(tg)
    }

    pub fn add_task(&mut self, task_id: TaskId) {
        if !self.node_map.contains_key(&task_id) {
            let idx = self.graph.add_node(task_id);
            self.node_map.insert(task_id, idx);
        }
    }

    pub fn add_dependency(&mut self, task_id: TaskId, depends_on: TaskId) -> SdkResult<()> {
        let from = self
            .node_map
            .get(&depends_on)
            .ok_or(SdkError::TaskNotFound {
                task_id: depends_on,
            })?;
        let to = self
            .node_map
            .get(&task_id)
            .ok_or(SdkError::TaskNotFound { task_id })?;

        self.graph.add_edge(*from, *to, ());
        Ok(())
    }

    pub fn check_cycles(&self) -> SdkResult<()> {
        match toposort(&self.graph, None) {
            Ok(_) => Ok(()),
            Err(cycle) => {
                let task_id = self.graph[cycle.node_id()];
                Err(SdkError::DependencyCycle {
                    task_ids: vec![task_id],
                })
            }
        }
    }

    pub fn topological_order(&self) -> SdkResult<Vec<TaskId>> {
        match toposort(&self.graph, None) {
            Ok(indices) => Ok(indices.into_iter().map(|idx| self.graph[idx]).collect()),
            Err(cycle) => {
                let task_id = self.graph[cycle.node_id()];
                Err(SdkError::DependencyCycle {
                    task_ids: vec![task_id],
                })
            }
        }
    }

    pub fn root_tasks(&self) -> Vec<TaskId> {
        self.graph
            .node_indices()
            .filter(|&idx| {
                self.graph
                    .neighbors_directed(idx, petgraph::Direction::Incoming)
                    .count()
                    == 0
            })
            .map(|idx| self.graph[idx])
            .collect()
    }

    pub fn dependents_of(&self, task_id: TaskId) -> Vec<TaskId> {
        if let Some(&idx) = self.node_map.get(&task_id) {
            self.graph
                .neighbors_directed(idx, petgraph::Direction::Outgoing)
                .map(|idx| self.graph[idx])
                .collect()
        } else {
            Vec::new()
        }
    }

    pub fn len(&self) -> usize {
        self.graph.node_count()
    }

    pub fn is_empty(&self) -> bool {
        self.graph.node_count() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn test_topological_order() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let id_c = Uuid::new_v4();

        let mut graph = TaskGraph::new();
        graph.add_task(id_a);
        graph.add_task(id_b);
        graph.add_task(id_c);

        graph.add_dependency(id_b, id_a).unwrap();
        graph.add_dependency(id_c, id_b).unwrap();

        let order = graph.topological_order().unwrap();
        let pos_a = order.iter().position(|&id| id == id_a).unwrap();
        let pos_b = order.iter().position(|&id| id == id_b).unwrap();
        let pos_c = order.iter().position(|&id| id == id_c).unwrap();

        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn test_cycle_detection() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        let mut graph = TaskGraph::new();
        graph.add_task(id_a);
        graph.add_task(id_b);

        graph.add_dependency(id_b, id_a).unwrap();
        graph.add_dependency(id_a, id_b).unwrap();

        assert!(graph.check_cycles().is_err());
    }
}