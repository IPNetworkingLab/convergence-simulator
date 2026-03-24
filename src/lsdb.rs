use std::collections::{HashMap, HashSet};

use crate::{
    Fib, MetricChange, Rid,
    graph::{collect_nodes, lsdb_dijkstra},
};

#[derive(Debug, Clone)]
pub struct LsaEntry {
    pub iface: String,
    pub metric: u64,
    pub latency: f64,
}

pub type Lsa = HashMap<Rid, LsaEntry>;
pub type Lsdb = HashMap<Rid, Lsa>;

/// Applies a set of metric changes to a router's LSDB in place.
pub fn apply_update(lsdb: &mut Lsdb, changes: &[MetricChange]) {
    for change in changes {
        lsdb.entry(change.src).and_modify(|lsa| {
            lsa.entry(change.dst).and_modify(|entry| {
                entry.metric = change.metric as u64;
            });
        });
    }
}

/// Computes the Forwarding Information Base for a given router from its LSDB view.
pub fn compute_fib(lsdb: &Lsdb, root: Rid) -> Option<Fib> {
    let (spt, cost) = lsdb_dijkstra(lsdb, root, false, true)?;
    let neighbors = spt.get(&root)?;
    let mut fib = Fib::new();
    for n in neighbors {
        let mut nodes = HashSet::new();
        collect_nodes(&spt, *n, &mut nodes);
        for dst in nodes {
            let dst_cost = *cost.get(&dst).unwrap() as u64;
            fib.entry(dst)
                .and_modify(|(_, nxhs)| nxhs.push(*n))
                .or_insert((dst_cost, vec![*n]));
        }
    }
    Some(fib)
}
