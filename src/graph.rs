use core::hash::Hash;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::{Lsa, Lsdb, Metric, Rid};

pub type WeightedNeighbors<T, U> = Vec<(T, U)>;
pub type WeightedHashSuccessors<T, U> = HashMap<T, WeightedNeighbors<T, U>>;

pub type Neighbors<T> = Vec<T>;
pub type HashSuccessors<T> = HashMap<T, Neighbors<T>>;

pub trait Successors<T, U> {
    fn successors(&self, from: &T) -> WeightedNeighbors<&T, &U>;
}

impl<T: Ord + Hash + Copy, U: Copy> Successors<T, U> for WeightedHashSuccessors<T, U> {
    fn successors(&self, from: &T) -> WeightedNeighbors<&T, &U> {
        self.get(from)
            .unwrap()
            .iter()
            .map(|(node, cost)| (node, cost))
            .collect()
    }
}

fn cast_lsa(lsa: &Lsa) -> WeightedNeighbors<Rid, Metric> {
    lsa.iter()
        .map(|(&rid, lsa)| (rid, lsa.metric as Metric))
        .collect()
}

pub fn cast_lsdb(lsdb: &Lsdb) -> WeightedHashSuccessors<Rid, Metric> {
    lsdb.iter()
        .map(|(&rid, lsa)| (rid, cast_lsa(lsa)))
        .collect()
}

pub trait DAG<T, U> {
    fn revert(&self) -> Self;
}
impl<T: Copy + Ord + Hash + std::fmt::Debug> DAG<T, u16> for WeightedHashSuccessors<T, u16> {
    fn revert(&self) -> Self {
        let mut ret: Self = Self::with_capacity(self.len());
        for (src, successors) in self.iter() {
            let mut line: WeightedNeighbors<T, u16> = WeightedNeighbors::new();
            for (dst, _) in successors {
                self[dst]
                    .iter()
                    .filter_map(|(child, cost)| {
                        if &child == &src {
                            Some((*dst, *cost))
                        } else {
                            None
                        }
                    })
                    .for_each(|x| line.push(x));
            }
            ret.insert(*src, line);
        }
        ret
    }
}

pub fn lsdb_dijkstra(
    lsdb: &Lsdb,
    root: Rid,
    reverse: bool,
    ecmp: bool,
) -> Option<(HashSuccessors<Rid>, HashMap<Rid, i32>)> {
    let mut graph = cast_lsdb(lsdb);
    if reverse {
        graph = graph.revert();
    }
    dijkstra(&graph, &root, ecmp).map(|(spt, cost)| (spt.reverse(), cost))
}

pub trait ReverseParenthood<T> {
    fn reverse(&self) -> Self;
}

impl<T: Ord + Hash + Copy> ReverseParenthood<T> for HashSuccessors<T> {
    fn reverse(&self) -> Self {
        let mut hash: Self = Self::new();
        self.iter().for_each(|(node, parents)| {
            parents.iter().for_each(|parent| {
                if parent != node {
                    hash.entry(*parent).or_default().push(*node)
                }
            })
        });
        hash
    }
}

pub fn collect_nodes<T: Ord + Hash + Copy>(g: &HashSuccessors<T>, n: T, nodes: &mut HashSet<T>) {
    nodes.insert(n);
    if let Some(neighbors) = g.get(&n) {
        neighbors
            .iter()
            .filter(|neighbor| *neighbor != &n)
            .for_each(|neighbor| collect_nodes(g, *neighbor, nodes));
    }
}

pub fn dijkstra<T: Ord + Hash + Copy>(
    graph: &WeightedHashSuccessors<T, u16>,
    start: &T,
    ecmp: bool,
) -> Option<(HashSuccessors<T>, HashMap<T, i32>)> {
    let mut heap: BinaryHeap<(i32, (&T, &T))> = BinaryHeap::new();
    let mut visited: HashSet<&T> = HashSet::new();
    let mut cost_to_reach: HashMap<&T, i32> = HashMap::new();
    let mut predecessors: HashMap<&T, Vec<&T>> = HashMap::new();

    heap.push((0, (start, start)));
    while let Some((cost, (current, from))) = heap.pop() {
        if visited.contains(current) {
            // ECMP: another path with the same optimal cost.
            if cost_to_reach.get(current).copied() == Some(cost) && ecmp {
                predecessors.entry(current).or_default().push(from);
            }
            continue;
        }

        visited.insert(current);
        predecessors.entry(current).or_default().push(from);
        cost_to_reach.insert(current, cost);

        for (neigh, local_cost) in graph
            .successors(current)
            .iter()
            .filter(|(neigh, _)| !visited.contains(neigh))
        {
            heap.push((cost - **local_cost as i32, (neigh, current)));
        }
    }

    Some((
        predecessors
            .iter()
            .map(|(key, value)| (**key, value.iter().map(|&v| *v).collect::<Vec<T>>()))
            .collect(),
        cost_to_reach
            .iter()
            .map(|(&key, value)| (*key, value.abs()))
            .collect(),
    ))
}
