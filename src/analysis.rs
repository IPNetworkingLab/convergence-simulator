use std::collections::{HashMap, HashSet};

use tracing::{debug, error, info, info_span};

use crate::fib::Fib;
use crate::graph::{HashSuccessors, lsdb_dijkstra};
use crate::lsdb::Lsdb;
use crate::{Rid, Update, apply_update};

#[derive(Debug, Clone)]
pub enum LinkType {
    Added,
    Removed,
    Both,
}

pub fn search_loops(
    supperposed: &HashMap<Rid, Vec<(Rid, LinkType)>>,
    loops: &mut HashSet<(Rid, Rid)>,
    explored: &mut HashSet<Rid>,
    node: &Rid,
) {
    explored.insert(*node);
    if let Some(children) = supperposed.get(node) {
        let l: HashSet<(Rid, Rid)> = children
            .iter()
            .filter_map(|(child, t)| {
                if let Some(grand_children) = supperposed.get(child) {
                    let grand_children: HashSet<&Rid> =
                        HashSet::from_iter(grand_children.iter().map(|(child, _)| child));
                    let (candidate, reverse) = if matches!(t, LinkType::Added) {
                        ((*node, *child), (*child, *node))
                    } else {
                        ((*child, *node), (*node, *child))
                    };
                    (grand_children.contains(node) && !loops.contains(&reverse))
                        .then_some(candidate)
                } else {
                    None
                }
            })
            .collect();
        if l.len() > 0 {
            loops.extend(l);
        }
        for (child, _) in children {
            if !explored.contains(child) {
                search_loops(supperposed, loops, explored, child);
            }
        }
    }
}

pub fn supperpose_dags(
    first: &HashSuccessors<Rid>,
    second: &HashSuccessors<Rid>,
    supperposed: &mut HashMap<Rid, Vec<(Rid, LinkType)>>,
    node: &Rid,
) {
    let first_children = first.get(node);
    let second_children = second.get(node);

    // Node was a parent in the old graph and became a leaf in the new graph.
    let children: Vec<(Rid, LinkType)> = if let Some(first_children) = first_children
        && second_children.is_none()
    {
        first_children
            .iter()
            .map(|child| (*child, LinkType::Removed))
            .collect()
    }
    // Node was a leaf in the old graph and is a parent in the new graph.
    else if let Some(second_children) = second_children
        && first_children.is_none()
    {
        second_children
            .iter()
            .map(|child| (*child, LinkType::Added))
            .collect()
    }
    // Node is still a parent, we have to compare children from both generations.
    else if let Some(first_children) = first_children
        && let Some(second_children) = second_children
    {
        let first_children: HashSet<&u32> = HashSet::from_iter(first_children);
        let mut second_children: HashSet<&u32> = HashSet::from_iter(second_children);
        let mut v: Vec<(Rid, LinkType)> = first_children
            .iter()
            .map(|child| {
                let t = if second_children.remove(child) {
                    LinkType::Both
                } else {
                    LinkType::Removed
                };
                (**child, t)
            })
            .collect();
        v.extend(
            second_children
                .iter()
                .map(|child| (**child, LinkType::Added)),
        );
        v
    } else {
        // Should not happen
        vec![]
    };

    if children.len() > 0 {
        supperposed.insert(*node, children.clone());
        for (child, _) in &children {
            if !supperposed.contains_key(&child) {
                supperpose_dags(first, second, supperposed, &child);
            }
        }
    }
}

pub fn static_analysis(
    lsdb: &Lsdb,
    root: Rid,
    update: &Update,
) -> Option<(
    HashSet<(u32, u32)>,
    HashSet<(u32, u32)>,
    HashSet<(u32, u32)>,
)> {
    debug!("starting static analaysis");
    // Static analysis.
    let Some((rspdag, _)) = lsdb_dijkstra(lsdb, root, true, true) else {
        error!("failed to compute rSPDAG from initial LSDB");
        return None;
    };
    // println!("rspdag g1 (gw): {rspdag:?}");

    let mut g2_lsdb = lsdb.clone();
    apply_update(&mut g2_lsdb, &update.forward);
    let g2_rspdag = lsdb_dijkstra(&g2_lsdb, root, true, true);
    if let Some((_g2_rspdag, _)) = &g2_rspdag {
        // println!("rspdag g2 (gw): {g2_rspdag:?}");
    }

    if let Some((_dag, _)) = lsdb_dijkstra(lsdb, 2, false, false) {
        // println!("dag(g2): {dag:?}");
    }

    if let Some((_dag, _)) = lsdb_dijkstra(lsdb, 2, false, true) {
        // println!("dag(g2) ecmp: {dag:?}");
    }

    let mut g3_lsdb = g2_lsdb.clone();
    apply_update(&mut g3_lsdb, &update.reverse);
    if let Some((g3_rspdag, _)) = lsdb_dijkstra(&g3_lsdb, root, true, true) {
        // println!("rspdag g3 (gw): {g3_rspdag:?}\n");

        // Collect loops for each type.
        if let Some((g2_rspdag, _)) = g2_rspdag {
            let mut g1g2 = Default::default();
            supperpose_dags(&rspdag, &g2_rspdag, &mut g1g2, &root);
            let mut loops_type1 = Default::default();
            search_loops(&g1g2, &mut loops_type1, &mut Default::default(), &root);
            // println!("g1g2: {g1g2:?}\nloops type1 {loops_type1:?}\n");

            let mut g2g3 = Default::default();
            supperpose_dags(&g2_rspdag, &g3_rspdag, &mut g2g3, &root);
            let mut loops_type2 = Default::default();
            search_loops(&g2g3, &mut loops_type2, &mut Default::default(), &root);
            // println!("g2g3: {g2g3:?}\nloops type2 {loops_type2:?}\n");

            let mut g1g3 = Default::default();
            supperpose_dags(&rspdag, &g3_rspdag, &mut g1g3, &root);
            let mut loops_type3 = Default::default();
            search_loops(&g1g3, &mut loops_type3, &mut Default::default(), &root);
            loops_type1.iter().chain(loops_type2.iter()).for_each(|l| {
                loops_type3.remove(l);
            });
            // println!("g1g3: {g1g3:?}\nloops type3 {loops_type3:?}\n");

            let mut outputs: HashSet<Rid> = Default::default();
            update.forward.iter().for_each(|change| {
                outputs.insert(change.src);
                outputs.insert(change.dst);
            });
            update.reverse.iter().for_each(|change| {
                outputs.insert(change.src);
                outputs.insert(change.dst);
            });
            outputs.remove(&root);

            pub fn solve_loop(head: &Rid, tail: &Rid, rspdag: &HashSuccessors<Rid>) {
                // print!("{tail} -> {head}: ");
                if let Some(children) = rspdag.get(tail)
                    && children.contains(head)
                {
                    // DAG contains the edge in the correct direction
                    // println!("contains and correct");
                } else if let Some(children) = rspdag.get(head)
                    && children.contains(tail)
                {
                    // DAG contains the edge in the reverse direction
                    // println!("contains and reverse");
                }
            }

            outputs.iter().for_each(|output| {
                if let Some((rspdag, _)) = lsdb_dijkstra(lsdb, *output, false, true) {
                    // println!("\ndag g1 ({output}): {rspdag:?}\n{:?}", rspdag.reverse());

                    // println!("Loops type 1");
                    let _ = loops_type1.iter().for_each(|(tail, head)| {
                        solve_loop(head, tail, &rspdag);
                    });

                    // println!("Loops type 2");
                    let _ = loops_type2.iter().for_each(|(tail, head)| {
                        solve_loop(head, tail, &rspdag);
                    });

                    // println!("Loops type 3");
                    let _ = loops_type3.iter().for_each(|(tail, head)| {
                        solve_loop(head, tail, &rspdag);
                    });
                }
            });
            Some((loops_type1, loops_type2, loops_type3))
        } else {
            None
        }
    } else {
        None
    }
}

pub type Path = Vec<Rid>;

#[derive(Debug)]
pub enum PathError {
    Blackhole { location: Rid },
    Loop { location: Rid },
}

#[derive(Debug)]
pub struct PathInfo {
    pub src: Rid,
    pub dst: Rid,
    pub path: Path,
    pub error: Option<PathError>,
}

/// Checks all FIBs in a step for blackholes and loops, returns paths to the
/// gateway as well as the FIB of each node.
pub fn check_fibs(
    fibs: &HashMap<Rid, Fib>,
    nodes: &HashSet<&Rid>,
    gw: &Rid,
) -> (bool, HashMap<Rid, Vec<Path>>) {
    let _span = info_span!("fibs check").entered();
    info!("starting FIBs checking");

    fn dfs(
        current: &Rid,
        src: &Rid,
        dst: &Rid,
        path: &mut Path,
        paths: &mut Vec<PathInfo>,
        errors: &mut u32,
        fibs: &HashMap<Rid, Fib>,
    ) {
        if !path.contains(current) {
            path.push(*current);
        } else {
            *errors += 1;
            error!(node = current, dest = dst, path = ?path, "routing loop detected");
            paths.push(PathInfo {
                src: *src,
                dst: *dst,
                path: path.clone(),
                error: Some(PathError::Loop { location: *current }),
            });
            return;
        }

        if dst == current {
            paths.push(PathInfo {
                src: *src,
                dst: *dst,
                path: path.clone(),
                error: None,
            });
            return;
        }

        if let Some(fib) = fibs.get(current)
            && let Some((_, nxhs)) = fib.get(dst)
        {
            for nxh in nxhs {
                dfs(nxh, src, dst, &mut path.clone(), paths, errors, fibs);
            }
        } else {
            *errors += 1;
            error!(node = current, src = src, dest = dst, "blackhole detected");
            paths.push(PathInfo {
                src: *src,
                dst: *dst,
                path: path.clone(),
                error: Some(PathError::Blackhole { location: *current }),
            });
        }
    }

    let mut all_errors = 0;
    let gw_paths: HashMap<Rid, Vec<Path>> = nodes
        .iter()
        .map(|src| {
            let mut errors = 0;
            let gw_paths: Vec<Path> = nodes
                .iter()
                .filter_map(|dst| {
                    if src == dst {
                        return None;
                    }
                    let mut paths = Vec::new();
                    // trace!(?src, ?dst, "exploring paths");
                    dfs(
                        src,
                        src,
                        dst,
                        &mut Vec::new(),
                        &mut paths,
                        &mut errors,
                        &fibs,
                    );
                    (*dst == gw).then_some(paths.into_iter().map(|p| p.path).collect::<Vec<Path>>())
                })
                .flatten()
                .collect();
            all_errors += errors;
            if errors == 0 {
                // trace!(node = src, "all destinations reachable");
            }
            (**src, gw_paths)
        })
        .collect();

    if all_errors == 0 {
        info!("step is transient-free");
    }

    (all_errors == 0, gw_paths)
}
