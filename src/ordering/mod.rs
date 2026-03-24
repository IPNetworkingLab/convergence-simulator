pub mod nested_forward;
pub mod sequential;

use std::collections::{HashMap, HashSet};

use tracing::{debug, debug_span, error, info, trace, warn};

use crate::{
    Event, Lsdb, Rid, Timeline, Timestamp, Update, apply_update, compare_fibs, compute_fib,
    graph::ReverseParenthood, lsdb_dijkstra, model::*,
};

pub enum Error {
    A,
}

/// Add events related to the delivery of a message in the history.
pub fn deliver_msg(
    src: &Rid,
    dst: &Rid,
    lsdb: &Lsdb,
    evt: Event,
    tx_ts: u64,
    times: &mut HashMap<u64, HashMap<Rid, Vec<Event>>>,
) -> Option<u64> {
    // Get latency toward neighbor.
    if let Some(lsa) = lsdb.get(src)
        && let Some(entry) = lsa.get(dst)
    {
        if let Some(tx_evt) = if matches!(evt, Event::UpdateRx(_)) {
            Some(Event::UpdateTx(*dst))
        } else if matches!(evt, Event::ConfRx(_)) {
            Some(Event::ConfTx(*dst))
        } else {
            None
        } {
            debug!(?tx_ts, ?tx_evt, "msg tx");
            times
                .entry(tx_ts)
                .or_default()
                .entry(*src)
                .or_default()
                .push(tx_evt);
        }

        let rx_ts = tx_ts + (entry.latency * 1000f64) as u64;
        debug!(?rx_ts, rx_evt=?evt, "msg rx");
        times
            .entry(rx_ts)
            .or_default()
            .entry(*dst)
            .or_default()
            .push(evt);
        Some(rx_ts)
    } else {
        None
    }
}

pub fn reverse_ordering(
    lsdb: &Lsdb,
    root: Rid,
    update: &Update,
    delay: Option<Timestamp>,
    nested: bool,
    times: &mut HashMap<Timestamp, HashMap<Rid, Vec<Event>>>,
) {
    fn bfs(
        ns: &Vec<Rid>,
        dag: &HashMap<Rid, Vec<Rid>>,
        lsdb: &Lsdb,
        update: &Update,
        start_ts: Option<Timestamp>,
        nested: bool,
        iteration: usize,
        times: &mut HashMap<Timestamp, HashMap<Rid, Vec<Event>>>,
        confs: &mut HashMap<Rid, HashMap<Rid, Timestamp>>,
    ) {
        debug!(?ns);
        let mut next_generation: HashSet<Rid> = Default::default();
        for n in ns {
            let _span = debug_span!("bfs", node=?n).entered();
            // Find last ts/event that happened on n. To do so, sort timestamps by
            // decreasing order.
            let mut clock = clock_from_timeline(n, times);

            // If we are partially nested, we start as soon as we finished the forward update.
            if let Some(start_ts) = start_ts
                && !nested
            {
                clock = clock.max(start_ts);
            }

            // 1. Ensure that we received every parents confirmation first.
            if let Some(parents) = dag.reverse().get(n) {
                let parents: HashSet<&Rid> = HashSet::from_iter(parents);
                if let Some((still_waiting, last_conf)) =
                    confs.get(n).and_then(|my_confs: &HashMap<Rid, Timestamp>| {
                        debug!(?parents, ?my_confs);
                        let still_waiting: HashSet<&Rid> = parents
                            .difference(&HashSet::from_iter(my_confs.keys()))
                            .cloned()
                            .collect();
                        Some((still_waiting, my_confs.values().max()))
                    })
                {
                    if !still_waiting.is_empty() {
                        debug!(missing=?still_waiting, "waiting for ECMP parents");
                        continue;
                    }
                    if let Some(last_conf) = last_conf {
                        clock = clock.max(*last_conf);
                    }
                }
            }
            let _span2 = debug_span!("", initial = clock).entered();

            debug!(?clock, "computing the SPT");
            let n_nodes = lsdb.keys().len() as f64;
            // let spt_duration = SPT_COMP_PER_NODE * lsdb.keys().len() as u64;
            let spt_duration = (SPT_COMP_PER_NODE * n_nodes * n_nodes.log2()) as u64;
            debug!(?spt_duration, "estimated SPT duration [µs]");
            clock += spt_duration;

            // if !(nested && iteration == 0) {
            // If we are nested and at the first iteration, we are the leaves of the dag.
            // In that case, the forward ordering already applied the reverse update.
            // Hence, we do not apply it a second time.
            //
            // 2. Apply the update
            debug!(?clock, "applying update");

            let mut new_lsdb = lsdb.clone();
            apply_update(&mut new_lsdb, &update.reverse);

            // Determine the FIB changes triggered by the update.
            if let Some(old_fib) = compute_fib(lsdb, *n)
                && let Some(new_fib) = compute_fib(&new_lsdb, *n)
            {
                // FIXME: FIB changes could be emitted in different orders accross runs.
                let changes = compare_fibs(&old_fib, &new_fib, n);
                for change in &changes {
                    clock += UPDATE_FIB_ENTRY;
                    times
                        .entry(clock)
                        .or_default()
                        .entry(*n)
                        .or_default()
                        .push(Event::FibUpdate(change.clone()));
                }
                let overhead = (changes.len() as u64) * UPDATE_FIB_ENTRY;
                debug!(?overhead, "estimated FIB overhead [µs]");
            }
            times
                .entry(clock)
                .or_default()
                .entry(*n)
                .or_default()
                .push(Event::UpdateApplied(crate::Direction::Reverse));
            debug!(?clock, "update applied");
            next_generation.remove(n);
            // }

            // 3. Send confirmation to ECMP children.
            if let Some(children) = dag.get(n) {
                let mut children = children.clone();
                children.sort();
                children.iter().for_each(|child| {
                    let evt = Event::ConfRx(*n);
                    clock += CONFIRMATION_TIME;
                    if let Some(rx_ts) = deliver_msg(n, child, lsdb, evt, clock, times) {
                        confs.entry(*child).or_default().insert(*n, rx_ts);
                    }
                });
                next_generation.extend(children);
            }
        }

        if !next_generation.is_empty() {
            bfs(
                &next_generation.iter().cloned().collect(),
                dag,
                lsdb,
                update,
                start_ts,
                nested,
                iteration + 1,
                times,
                confs,
            );
        }
    }

    if let Some((dag, _)) = lsdb_dijkstra(lsdb, root, true, true)
    // && let Some((tree, _)) = lsdb_dijkstra(lsdb, root, true, false)
    {
        let nodes: HashSet<Rid> = lsdb.keys().cloned().collect();
        let parents = HashSet::from_iter(dag.keys().cloned());
        let mut leaves: Vec<Rid> = nodes.difference(&parents).cloned().collect();
        leaves.sort();
        // println!("dparents: {parents:?}\nleaves {leaves:?}\nroot {root}");

        // println!(
        //     "root: {root}\ndag: {dag:?}\ntree: {tree:?}\nparents: {parents:?}\nleaves: {leaves:?}\nreverse dag: {:?}",
        //     dag.reverse()
        // );
        let mut start_ts = times.keys().max().cloned();
        if let Some(delay) = delay
            && let Some(start_ts) = &mut start_ts
        {
            *start_ts += delay;
        }

        bfs(
            &leaves,
            &dag.reverse(),
            lsdb,
            update,
            start_ts,
            nested,
            0,
            times,
            &mut Default::default(),
        );
    }
}

pub enum ForwardType {
    ForwardOnly,
    ForwardOrder,
    FullyNested,
}

fn clock_from_timeline(node: &Rid, timeline: &Timeline) -> Timestamp {
    let mut timestamps: Vec<&Timestamp> = timeline.keys().collect();
    timestamps.sort();
    timestamps.reverse();
    **timestamps
        .iter()
        .find(|ts| timeline[ts].contains_key(node))
        .or(Some(&&0))
        .unwrap()
}

pub fn forward_ordering(
    lsdb: &Lsdb,
    root: Rid,
    update: &Update,
    nesting: ForwardType,
    timeline: Option<Timeline>,
    start_last: bool,
) -> Result<Timeline, Error> {
    info!("computing forward ordering");

    fn bfs(
        // Nodes at the current depth to analyse.
        ns: Vec<Rid>,
        // ECMP children mapped to their parents.
        wl: &HashMap<Rid, Vec<Rid>>,
        // DAG rooted at root.
        dag: &HashMap<Rid, Vec<Rid>>,
        // A rSPTree rooted at root.
        tree: &HashMap<Rid, Vec<Rid>>,
        // The initial LSDB common to all nodes before the update.
        lsdb: &Lsdb,
        // The root of the update.
        root: &Rid,
        // The actual topological update.
        update: &Update,
        // Do we apply nested updates in the Forward ordering?
        nesting: ForwardType,
        // The actual network update history.
        times: &mut HashMap<u64, HashMap<Rid, Vec<Event>>>,
        // Mapping between nodes and the instant at which they have received the
        // update.
        arrival: &mut HashMap<Rid, (Rid, u64)>,
        // Mapping between nodes and the confirmation they received from their
        // parents with the reception timestamp.
        confs: &mut HashMap<u32, HashMap<Rid, u64>>,
        // Mapping between nodes to resume and the list of blocking nodes.
        to_resume: &mut HashMap<Rid, HashSet<Rid>>,
        start_ts: Option<Timestamp>,
    ) {
        let mut next_generation: Vec<Rid> = vec![];
        for n in &ns {
            let _span = debug_span!("bfs", node=?n).entered();
            let fib = compute_fib(lsdb, *n).unwrap();

            let mut clock = clock_from_timeline(n, times);
            if let Some(start_ts) = start_ts {
                clock = clock.max(start_ts);
            }

            let (parent, _) = arrival[n];

            // 1. Compute waiting list.
            // Am I an ECMP child?
            // Get all nexthops to reach the root of the DAG.
            // debug!("testing whether ECMP child");
            if let Some((_cost, parents)) = fib.get(root)
                && parents.len() > 1
                // If we only distribute in multicast a given update, we do not apply ordering.
                && !matches!(nesting, ForwardType::ForwardOnly)
            {
                let _span2 = debug_span!("ecmp_child").entered();
                // 2. Wait for each entry in the waiting list.
                // I have to wait each parent before forwarding the update to my children.
                let mut parents: HashSet<&u32> = HashSet::from_iter(parents);
                debug!(?parents, "ecmp");

                // Remove confirmations already received.
                parents.remove(&parent);
                let max_conf = confs.get(n).and_then(|my_confs: &HashMap<u32, u64>| {
                    for conf in my_confs.keys() {
                        parents.remove(conf);
                    }
                    my_confs.values().max()
                });
                debug!(?parents, "remaining ecmp");

                // We didn't receive every confirmation yet!
                if parents.len() != 0 {
                    warn!("ecmp parents remaining: must backtrack later!");
                    to_resume.insert(*n, parents.iter().cloned().cloned().collect());
                    continue;
                }

                // We resume the node when every confirmation has been received.
                let old_clock = clock;
                if let Some(max_conf) = max_conf {
                    clock = clock.max(*max_conf);
                }
                debug!(
                    "no remaining ecmp parent, setting current node time to {clock} while update has been received at {old_clock}"
                );
            }
            let _span2 = debug_span!("", initial=?clock).entered();

            // 3. Apply the update iif we order an update.
            if !matches!(nesting, ForwardType::ForwardOnly) {
                let mut reverse = false;
                let changes = match nesting {
                    // Apply only forward changes.
                    ForwardType::ForwardOrder => update.forward.clone(),
                    // Apply forward and reverse changes at once.
                    ForwardType::FullyNested => {
                        reverse = true;
                        update.into()
                    }
                    // Never reached.
                    _ => {
                        error!("Unexpected forward type.");
                        vec![]
                    }
                };
                if !changes.is_empty() {
                    debug!(?clock, "computing the SPT");
                    let n_nodes = lsdb.keys().len() as f64;
                    // let spt_duration = SPT_COMP_PER_NODE * lsdb.keys().len() as u64;
                    let spt_duration = (SPT_COMP_PER_NODE * n_nodes * n_nodes.log2()) as u64;
                    debug!(?spt_duration, "estimated SPT duration [µs]");
                    clock += spt_duration;

                    debug!(?clock, "applying update");
                    let mut new_lsdb = lsdb.clone();
                    apply_update(&mut new_lsdb, &changes);

                    // Determine the FIB changes triggered by the update.
                    let old_fib = fib;
                    if let Some(new_fib) = compute_fib(&new_lsdb, *n) {
                        // FIXME: FIB changes could be emitted in different orders accross runs.
                        let changes = compare_fibs(&old_fib, &new_fib, n);
                        for change in &changes {
                            clock += UPDATE_FIB_ENTRY;
                            times
                                .entry(clock)
                                .or_default()
                                .entry(*n)
                                .or_default()
                                .push(Event::FibUpdate(change.clone()));
                        }
                        let overhead = (changes.len() as u64) * UPDATE_FIB_ENTRY;
                        debug!(?overhead, "estimated FIB overhead [µs]");
                    }
                    let evts = times.entry(clock).or_default().entry(*n).or_default();
                    evts.push(Event::UpdateApplied(crate::Direction::Forward));
                    if reverse {
                        evts.push(Event::UpdateApplied(crate::Direction::Reverse))
                    }
                    debug!(?clock, "update applied");
                }
            }

            // 4. Replicate the update to each of my non-ECMP child.
            if let Some(tree_neighbors) = tree.get(n) {
                debug!("replicating update to tree children");

                // Update next generation for BFS.
                next_generation.extend(tree_neighbors);

                // Tie-break in BFS.
                let mut neighbors = tree_neighbors.clone();
                neighbors.sort();

                for neighbor in neighbors.iter() {
                    let _span3 = debug_span!("replication", child=?neighbor).entered();
                    clock += REPLICATION_TIME;
                    if let Some(rx_ts) =
                        deliver_msg(n, neighbor, lsdb, Event::UpdateRx(*n), clock, times)
                    {
                        arrival.insert(*neighbor, (*n, rx_ts));
                    }
                }
            }

            // 5. Sends confirmation to ECMP children.
            // Am I an ECMP parent?
            if !matches!(nesting, ForwardType::ForwardOnly)
                && let Some(dag_neighbors) = dag.get(n)
            {
                let a: HashSet<&u32> = HashSet::from_iter(dag_neighbors.iter());
                let b: HashSet<&u32> = HashSet::from_iter(wl.keys());
                let ecmp_children: HashSet<&u32> = a.intersection(&b).cloned().collect();
                let mut ecmp_children: Vec<&u32> = if let Some(neighbors) = tree.get(n) {
                    let c: HashSet<&u32> = HashSet::from_iter(neighbors.iter());
                    // Collect ECMP \ non-ECMP children.
                    ecmp_children.difference(&c).cloned().collect()
                } else {
                    ecmp_children.iter().cloned().collect()
                };

                // Tie-break for reproducibility.
                ecmp_children.sort();

                if !ecmp_children.is_empty() {
                    debug!("sending ECMP confirmations");
                }
                for &child in ecmp_children.iter() {
                    let _span3 = debug_span!("confirmation", ?child).entered();
                    clock += CONFIRMATION_TIME;
                    if let Some(rx_ts) =
                        deliver_msg(n, child, lsdb, Event::ConfRx(*n), clock, times)
                    {
                        confs.entry(*child).or_default().insert(*n, rx_ts);

                        // Verify if it unlocks a child for which we are backtracking.
                        debug!("testing child unlocking");
                        to_resume.entry(*child).and_modify(|entry| {
                            entry.remove(n);
                            debug!(parents=?entry, "parents remaining");
                            if entry.is_empty() {
                                // We reprogram the newly unlocked node.
                                debug!("repogramming backtracked node");
                                next_generation.push(*child);
                            }
                        });
                    }
                }
            }
        }
        if !next_generation.is_empty() {
            bfs(
                next_generation,
                wl,
                dag,
                tree,
                lsdb,
                root,
                update,
                nesting,
                times,
                arrival,
                confs,
                to_resume,
                start_ts,
            );
        }
    }

    if let Some((dag, _)) = lsdb_dijkstra(lsdb, root, false, true)
        && let Some((tree, _)) = lsdb_dijkstra(lsdb, root, false, false)
    {
        let waiters: HashMap<Rid, Vec<Rid>> = dag
            .reverse()
            .iter()
            .filter_map(|(child, parents)| (parents.len() > 1).then_some((*child, parents.clone())))
            .collect();

        let mut timeline: Timeline = if let Some(timeline) = timeline {
            timeline
        } else {
            let mut times: Timeline = Default::default();
            times.insert(
                0,
                HashMap::from_iter(vec![(root, vec![Event::UpdateRx(root)])]),
            );
            times
        };

        let mut arrivals: HashMap<Rid, (Rid, Timestamp)> = Default::default();
        arrivals.insert(root, (root, 0));
        let last_ts = *timeline.keys().max().unwrap();
        bfs(
            vec![root],
            &waiters,
            &dag,
            &tree,
            lsdb,
            &root,
            update,
            nesting,
            &mut timeline,
            &mut arrivals,
            &mut Default::default(),
            &mut Default::default(),
            start_last.then_some(last_ts),
        );
        trace!(?arrivals, "arrivals");
        Ok(timeline)
    } else {
        warn!("Failed to compute dijkstra on LSDB");
        Err(Error::A)
    }
}
