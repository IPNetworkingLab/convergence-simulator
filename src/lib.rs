use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::{net::IpAddr, str::FromStr};

use dune_core::Dune;
use serde::Serialize;
use tracing::{debug, error, info, info_span, warn};

mod analysis;
mod fib;
mod graph;
mod lsdb;
mod ordering;

use crate::analysis::*;
use crate::fib::*;
use crate::graph::lsdb_dijkstra;
use crate::lsdb::*;
use crate::ordering::nested_forward::nested_forward_ordering;
use crate::ordering::sequential::sequential_ordering;

// Based on OSPF values.
pub type Rid = u32;
pub type Metric = u16;

pub type Step = HashMap<Rid, Lsdb>;
pub type Timestamp = u64;
pub type Timeline = HashMap<Timestamp, HashMap<Rid, Vec<Event>>>;

pub mod model {
    pub const REPLICATION_TIME: u64 = 10;
    pub const CONFIRMATION_TIME: u64 = 5;
    pub const UPDATE_FIB_ENTRY: u64 = 100;
    // pub const SPT_COMP_PER_NODE: u64 = 45;
    pub const SPT_COMP_PER_NODE: f64 = 0.039;
}

#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)]
/// Events that can happen in the network
pub enum Event {
    /// Send an update to rid
    UpdateTx(Rid),
    /// Deliver an updated from rid
    UpdateRx(Rid),
    /// The update has been locally applied
    UpdateApplied(Direction),
    /// FIB update
    FibUpdate(FibChange),
    /// The FIB has been entirely synced
    FibSynced,
    /// Send a confirmation to rid
    ConfTx(Rid),
    /// Deliver a confirmation from rid
    ConfRx(Rid),
}

#[derive(Debug, Clone)]
pub struct MetricChange {
    pub src: Rid,
    pub dst: Rid,
    pub metric: Metric,
}

#[derive(Debug, Clone)]
pub struct Update {
    pub forward: Vec<MetricChange>,
    pub reverse: Vec<MetricChange>,
    pub root: Rid,
}

impl Update {
    pub fn new(root: Rid) -> Self {
        Self {
            root,
            forward: Default::default(),
            reverse: Default::default(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Clone)]
pub enum Direction {
    Forward,
    Reverse,
}

// 0.65535:3
impl TryFrom<&str> for MetricChange {
    type Error = ();
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let (nodes, metric) = value.split_once(':').ok_or_else(|| {
            error!(input = value, "malformed metric change");
        })?;
        let metric = u16::from_str(metric).map_err(|_| {
            error!(raw = metric, "metric value is not a valid u16");
        })?;
        let (src, dst) = nodes.split_once('.').ok_or(())?;
        let src = u32::from_str(src).map_err(|_| ())?;
        let dst = u32::from_str(dst).map_err(|_| ())?;
        Ok(MetricChange { src, dst, metric })
    }
}

impl From<&Update> for Vec<MetricChange> {
    fn from(update: &Update) -> Self {
        update
            .forward
            .iter()
            .chain(&update.reverse)
            .cloned()
            .collect()
    }
}

pub fn parse_metric_changes(s: &str) -> Vec<MetricChange> {
    let changes: Vec<MetricChange> = s
        .split(';')
        .filter_map(|change| MetricChange::try_from(change).ok())
        .collect();
    debug!(count = changes.len(), "metric changes parsed");
    changes
}

/// Loads a topology file and returns the node name:RID mapping and the initial LSDB.
pub fn load_topology(path: &PathBuf) -> (HashMap<String, Rid>, Lsdb) {
    let _span = info_span!("load_topology", path = %path.display()).entered();
    let dune = Dune::new(path, false);
    load_initial_state(dune)
}

pub fn load_initial_state(dune: Dune) -> (HashMap<String, Rid>, Lsdb) {
    let names: HashMap<String, Rid> = dune
        .nodes
        .iter()
        .filter_map(|(nid, node)| {
            if let Some(af) = &node._additional_fields
                && let Some(rid) = af.get("rid")
                && let Some(rid) = rid.as_str()
                && let Ok(IpAddr::V4(rid)) = IpAddr::from_str(rid)
            {
                Some((nid.clone(), rid.to_bits()))
            } else {
                None
            }
        })
        .collect();

    let lsdb: Lsdb = dune
        .nodes
        .iter()
        .filter_map(|(nid, node)| {
            if let Some(ifaces) = &node.interfaces
                && let Some(rid) = names.get(nid)
            {
                let lsa: Lsa = ifaces
                    .iter()
                    .filter_map(|(iface, cfg)| {
                        cfg.peer_name.as_ref().and_then(|peer| {
                            if let Some(metric) = cfg.metric
                                && let Some(latency) = cfg.latency
                                && let Some(peer) = names.get(peer)
                            {
                                Some((
                                    *peer,
                                    LsaEntry {
                                        iface: iface.clone(),
                                        metric,
                                        latency,
                                    },
                                ))
                            } else {
                                error!(iface = %iface, "interface missing metric or latency, skipping");
                                None
                            }
                        })
                    })
                    .collect();
                Some((*rid, lsa))
            } else {
                None
            }
        })
        .collect();

    info!(nodes = lsdb.len(), "LSDB loaded");
    // trace!("LSDB: {lsdb:#?}");

    (names, lsdb)
}

#[derive(Serialize, Debug)]
pub enum EmitEvent {
    Update {
        rid: Rid,
        ts: f64,
        direction: Direction,
    },
    FibUpdate {
        ts: f64,
        node: Rid,
        seamless: Option<bool>,
    },
    Confirmation {
        ts: f64,
    },
    UpdateTx {
        ts: f64,
    },
    Loops {
        type_1: HashSet<(u32, u32)>,
        type_2: HashSet<(u32, u32)>,
        type_3: HashSet<(u32, u32)>,
    },
}

pub fn run_timeline(
    gw: &Rid,
    lsdb: &Lsdb,
    nodes: &HashSet<&Rid>,
    timeline: &Timeline,
    check: bool,
) -> Vec<EmitEvent> {
    let _span = info_span!("run").entered();

    let mut fibs: HashMap<Rid, Fib> = nodes
        .iter()
        .map(|&&node| (node, compute_fib(lsdb, node).unwrap()))
        .collect();

    let mut first_fib_update: Option<u64> = None;
    let mut last_fib_update: u64 = 0;

    // For each FIB update in the timeline, we check all FIBs for transient events.
    let mut updated_forward: HashSet<&Rid> = Default::default();
    let mut updated_reverse: HashSet<&Rid> = Default::default();
    let mut order: Vec<Timestamp> = timeline.keys().cloned().collect();
    order.sort();
    let mut emit_events: Vec<EmitEvent> = Default::default();
    for ts in &order {
        for (node, evts) in &timeline[&ts] {
            info!(?ts, ?node, ?evts);
            if evts.contains(&Event::UpdateApplied(Direction::Forward)) {
                updated_forward.insert(node);
            }
            if evts.contains(&Event::UpdateApplied(Direction::Reverse)) {
                updated_reverse.insert(node);
            }
            let emit: Vec<EmitEvent> = evts
                .iter()
                // .filter(|evt| matches!(evt, Event::FibUpdate(_)))
                .filter_map(|upd| {
                    if let Event::FibUpdate(FibChange { dst, changes, .. }) = upd {
                        // Save first FIB update ts.
                        if first_fib_update.is_none() {
                            first_fib_update = Some(*ts);
                        }
                        last_fib_update = *ts;
                        for change in changes {
                            if let Some(lsdb) = fibs.get_mut(node) {
                                lsdb.entry(*dst).and_modify(|(cost, nxhs)| match change {
                                    FibChangeType::MetricChange(
                                        FibMetricChange::Increment { new, .. }
                                        | FibMetricChange::Decrement { new, .. },
                                    ) => {
                                        *cost = *new;
                                    }
                                    FibChangeType::NxhChange(FibNxhChange::Removal(nxh)) => {
                                        *nxhs = nxhs
                                            .iter()
                                            .filter_map(|old_nxh| {
                                                (old_nxh != nxh).then_some(old_nxh)
                                            })
                                            .cloned()
                                            .collect();
                                    }
                                    FibChangeType::NxhChange(FibNxhChange::Addition(nxh)) => {
                                        nxhs.push(*nxh);
                                    }
                                    FibChangeType::NxhChange(FibNxhChange::Swap { old, new }) => {
                                        *nxhs = nxhs
                                            .iter()
                                            .filter_map(|old_nxh| {
                                                (old_nxh != old).then_some(old_nxh).or(Some(new))
                                            })
                                            .cloned()
                                            .collect();
                                    }
                                    _ => {}
                                });
                            }
                        }
                        let seamless = if check {
                            let (seamless, _) = check_fibs(&fibs, nodes, gw);
                            Some(seamless)
                        } else {
                            None
                        };
                        let emit = EmitEvent::FibUpdate {
                            ts: *ts as f64,
                            node: *node,
                            seamless,
                        };
                        debug!(?emit, "emiting");
                        Some(emit)
                    } else if let Event::ConfTx(_) = upd {
                        Some(EmitEvent::Confirmation { ts: *ts as f64 })
                    } else if let Event::UpdateTx(_) = upd {
                        Some(EmitEvent::UpdateTx { ts: *ts as f64 })
                    } else if let Event::UpdateApplied(direction) = upd {
                        Some(EmitEvent::Update {
                            rid: *node,
                            ts: *ts as f64,
                            direction: direction.clone(),
                        })
                    } else {
                        None
                    }
                })
                .collect();
            emit_events.extend(emit);
        }
    }

    // Sanity check: ensure that every node has been updated.
    let diff_fwd: HashSet<&Rid> = nodes.difference(&updated_forward).cloned().collect();
    let diff_rev: HashSet<&Rid> = nodes.difference(&updated_reverse).cloned().collect();

    if !diff_fwd.is_empty() {
        error!(missing=?diff_fwd, "Not every node has applied the forward update!");
    }

    if !diff_rev.is_empty() {
        error!(missing=?diff_rev, "Not every node has applied the reverse update!");
    }

    if diff_fwd.is_empty() && diff_rev.is_empty() {
        info!("Every node has been updated!");
    };

    info!("C_FUT {}µs", last_fib_update - first_fib_update.unwrap());

    let convergence_duration = *(&order.last().unwrap()) - *(&order.first().unwrap());
    info!("Convergence duration {convergence_duration}µs");

    emit_events
}

/// Runs the full convergence simulation from an initial network step.
pub fn run_simulation(
    nodes: &HashSet<&Rid>,
    root: Rid,
    update: &Update,
    lsdb: &Lsdb,
    cfg: Vec<SimulationCfg>,
) -> HashMap<String, Vec<EmitEvent>> {
    let mut ret: HashMap<String, Vec<EmitEvent>> = Default::default();

    // Static analysis.
    if let Some((type_1, type_2, type_3)) = static_analysis(lsdb, root, update) {
        ret.insert(
            "Loops".to_string(),
            vec![EmitEvent::Loops {
                type_1,
                type_2,
                type_3,
            }],
        );
    }

    for sim_cfg in cfg {
        match sim_cfg.method {
            SynchronisationMethod::Nested => {
                // Test nested forward ordering.
                if let Some(change) = update.forward.first() {
                    if let Ok(timeline) = nested_forward_ordering(lsdb, change.src, update) {
                        let events = run_timeline(&root, lsdb, nodes, &timeline, sim_cfg.fib_check);
                        ret.insert("NestedForward".to_string(), events);
                    }
                } else {
                    warn!("No forward change");
                }
            }
            SynchronisationMethod::Sequential => {
                if let Ok(timeline) = sequential_ordering(
                    lsdb,
                    update.forward[0].src,
                    update.reverse[0].src,
                    update,
                    None,
                ) {
                    let events = run_timeline(&root, lsdb, nodes, &timeline, sim_cfg.fib_check);
                    ret.insert("Sequential".to_string(), events);
                }
            }
        }
    }

    return ret;
}

pub fn simulate(
    dune: Dune,
    root: Rid,
    add: Vec<MetricChange>,
    rm: Vec<MetricChange>,
    dump: Option<PathBuf>,
    gw_name: &String,
    ts: u64,
    cfg: Vec<SimulationCfg>,
) -> HashMap<String, Vec<EmitEvent>> {
    let mut filename = format!("{ts:05?}_{gw_name}");
    for MetricChange { src, .. } in &add {
        filename = format!("{filename}_add_{src}");
    }
    for MetricChange { src, .. } in &rm {
        filename = format!("{filename}_del_{src}");
    }

    if let Some(path) = &dump {
        let mut path = path.clone();
        path.push("log");
        if !path.exists() {
            let ret = std::fs::create_dir(&path);
            debug!(?ret);
        }
        path.push(filename.clone());
        path.set_extension("log");
        tracing::Span::current().record("log_file", path.to_str());
    }

    let (rid_map, lsdb) = load_initial_state(dune);
    let nodes: HashSet<&Rid> = rid_map.values().collect();
    let update = Update {
        root,
        forward: add,
        reverse: rm,
    };
    let events: HashMap<String, Vec<EmitEvent>> = run_simulation(&nodes, root, &update, &lsdb, cfg);
    if let Some(mut path) = dump
        && let Ok(content) = serde_json::to_string(&events)
    {
        path.push("simulation");
        if !path.exists() {
            let _ = std::fs::create_dir(path.clone());
        }
        path.push(filename);
        path.set_extension("json");
        let _ = std::fs::write(path.clone(), content);
    }
    events
}

use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
pub enum SynchronisationMethod {
    Sequential,
    Nested,
}

impl TryFrom<&str> for SynchronisationMethod {
    type Error = ();
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value == "Nested" {
            Ok(Self::Nested)
        } else if value == "Sequential" {
            Ok(Self::Sequential)
        } else {
            Err(())
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct SimulationCfg {
    pub method: SynchronisationMethod,
    pub fib_check: bool,
}
