use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use clap::Parser;
use tracing::{error, info, info_span};

use convergence_simulator::{
    EmitEvent, Rid, SimulationCfg, SynchronisationMethod, Update, load_topology,
    parse_metric_changes, run_simulation,
};

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long)]
    topology: PathBuf,
    #[arg(long)]
    root: Option<Rid>,
    #[arg(short, long)]
    forward: Option<String>,
    #[arg(short, long)]
    reverse: Option<String>,
    #[arg(short, long, default_value_t = true)]
    check_fib: bool,
    #[arg(short, long, default_value_t = false)]
    sequential: bool,
    #[arg(short, long, default_value_t = false)]
    nested: bool,
}

fn main() -> Result<(), ()> {
    // tracing_subscriber::fmt::init();
    info!("Starting simulator.");

    let args = Args::parse();
    if !args.topology.exists() {
        error!("Topology path does not exist.");
        return Err(());
    }
    if !args.topology.is_file() {
        error!("Topology path does not point to a file.");
        return Err(());
    }

    let (names, lsdb) = load_topology(&args.topology);
    let nodes: HashSet<&Rid> = names.values().collect();

    let mut cfg: Vec<SimulationCfg> = Default::default();
    if args.nested {
        cfg.push(SimulationCfg {
            method: SynchronisationMethod::Nested,
            fib_check: args.check_fib,
        });
    }
    if args.sequential {
        cfg.push(SimulationCfg {
            method: SynchronisationMethod::Sequential,
            fib_check: args.check_fib,
        });
    }

    if let Some(root) = args.root {
        let mut update = Update::new(root);
        if let Some(changes) = args.forward {
            let _span = info_span!("forward").entered();
            update.forward = parse_metric_changes(&changes);
        }
        if let Some(changes) = args.reverse {
            let _span = info_span!("reverse").entered();
            update.reverse = parse_metric_changes(&changes);
        }
        let events: HashMap<String, Vec<EmitEvent>> =
            run_simulation(&nodes, root, &update, &lsdb, cfg);
        if let Ok(content) = serde_json::to_string(&events) {
            let mut json = args.topology.clone();
            json.set_extension("json");
            let _ = std::fs::write(json, content);
        }
    }

    Ok(())
}
