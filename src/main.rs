use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use ltx_klusta::concurrency::JobCount;
use ltx_klusta::dump::Point;
use ltx_klusta::queue::{ConsoleResult, Queue};
use ltx_klusta::{pipeline, Error, Features, Result, MAX_BLOCK_SPIKES};
use std::collections::VecDeque;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Mutex;

/// Spike sorting for tetrode recordings: PCA features, then clustering with a
/// Rust port of KlustaKwik. Writes NAME.clu.N beside each NAME.N recording.
#[derive(Debug, Parser)]
#[command(
    name = "ltx-klusta",
    bin_name = "ltxk",
    version,
    args_conflicts_with_subcommands = true,
    after_help = "EXAMPLES:\n  ltxk recording.1 recording.2       Queue recordings and watch progress\n  ltxk                               Open the queue\n  ltxk cluster recording.1 --no-queue\n  ltxk cluster recording.1 --dump features --dump-only\n  ltxk status\n\nDropping recordings onto the executable queues them with the default settings."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Recordings to queue (drag-and-drop). With none, open the queue.
    #[arg(value_hint = clap::ValueHint::FilePath)]
    inputs: Vec<PathBuf>,
    /// Write shell completion definitions and exit
    #[arg(long, value_enum, exclusive = true)]
    completions: Option<Shell>,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)] // Parsed once per process.
enum Command {
    /// Submit a selection from the Windows Explorer launcher without a console
    #[command(hide = true)]
    Enqueue {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        queue_dir: Option<PathBuf>,
    },
    /// Cluster recordings through the shared queue, showing progress
    Cluster {
        #[arg(required = true, num_args = 1.., value_hint = clap::ValueHint::FilePath)]
        inputs: Vec<PathBuf>,
        #[command(flatten)]
        options: ClusterOptions,
        #[command(flatten)]
        mode: Mode,
    },
    /// Process waiting or interrupted jobs, showing progress
    Resume {
        /// Maximum simultaneous recordings: auto or 1–16 (default: ltxk.json or auto)
        #[arg(long, value_name = "AUTO|N")]
        jobs: Option<JobCount>,
        #[arg(long)]
        queue_dir: Option<PathBuf>,
        /// Process without a progress display (used for --background)
        #[arg(long, hide = true)]
        headless: bool,
    },
    /// List queued, running, finished and failed jobs, with log locations
    Status {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        queue_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Default, Args)]
struct Mode {
    /// Process in this command only, without the shared queue
    #[arg(long, conflicts_with_all = ["background", "queue_dir"])]
    no_queue: bool,
    /// Queue and return immediately; a background process does the work
    #[arg(long)]
    background: bool,
    /// Use this queue folder instead of the per-user default
    #[arg(long)]
    queue_dir: Option<PathBuf>,
}

#[derive(Debug, Default, Args)]
struct ClusterOptions {
    /// JSON settings file (default: ltxk.json beside the executable, if present)
    #[arg(long)]
    config: Option<PathBuf>,
    /// Features to cluster on, e.g. 3PC or 3PC,A,VT (default: 3PC)
    #[arg(long, value_parser = parse_features)]
    features: Option<String>,
    /// Smallest initial cluster count, including noise (default: 20)
    #[arg(long, value_parser = clap::value_parser!(u32).range(2..100))]
    min_clusters: Option<u32>,
    /// Largest initial cluster count, including noise (default: 30)
    #[arg(long, value_parser = clap::value_parser!(u32).range(2..100))]
    max_clusters: Option<u32>,
    /// Approximate number of spikes to fit; the rest are labelled by the fit (default: 25000)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=25_000))]
    target_spikes: Option<u32>,
    /// Random seed (default: 1)
    #[arg(long)]
    seed: Option<u32>,
    /// Maximum EM iterations per fit (default: 10000)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100_000))]
    max_iterations: Option<u32>,
    /// Maximum simultaneous recordings: auto or 1–16 (default: auto)
    #[arg(long, value_name = "AUTO|N")]
    jobs: Option<JobCount>,
    /// Replace existing .clu.N files (otherwise skip them)
    #[arg(long)]
    force: bool,
    /// Scratch folder on a disk with room for temporary features
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Spikes per waveform block when reading recordings (default: 4096)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=MAX_BLOCK_SPIKES as i64))]
    block_spikes: Option<u32>,
    /// Write intermediate data as .npy files (comma-separated)
    #[arg(long, value_enum, value_delimiter = ',')]
    dump: Vec<Point>,
    /// Folder for dumps, one subfolder per recording (default: NAME.dump.N beside each recording)
    #[arg(long)]
    dump_dir: Option<PathBuf>,
    /// Stop once the requested dumps are written; do not write .clu files
    #[arg(long)]
    dump_only: bool,
}

fn parse_features(text: &str) -> std::result::Result<String, String> {
    Features::parse(text)
        .map(|_| text.to_owned())
        .map_err(|error| error.to_string())
}

fn absolute(path: PathBuf) -> Result<PathBuf> {
    Ok(if path.is_relative() {
        std::env::current_dir()?.join(path)
    } else {
        path
    })
}

impl ClusterOptions {
    fn resolve(self) -> Result<pipeline::Settings> {
        let mut settings = pipeline::Settings::load(self.config.as_deref())?;
        let cluster = &mut settings.cluster;
        if let Some(features) = self.features {
            settings.features = features;
        }
        if let Some(value) = self.min_clusters {
            cluster.min_clusters = value as usize;
        }
        if let Some(value) = self.max_clusters {
            cluster.max_clusters = value as usize;
        }
        if let Some(value) = self.target_spikes {
            cluster.target_spikes = value as usize;
        }
        if let Some(value) = self.seed {
            cluster.seed = value;
        }
        if let Some(value) = self.max_iterations {
            cluster.max_iterations = value as usize;
        }
        if let Some(value) = self.jobs {
            settings.jobs = value.0;
        }
        if let Some(value) = self.block_spikes {
            settings.block_spikes = value as usize;
        }
        settings.force |= self.force;
        if let Some(path) = self.work_dir {
            settings.work_dir = Some(absolute(path)?);
        }
        if !self.dump.is_empty() {
            settings.dump = self.dump;
        }
        if let Some(path) = self.dump_dir {
            settings.dump_dir = Some(absolute(path)?);
        }
        settings.dump_only |= self.dump_only;
        settings.validate()?;
        Ok(settings)
    }
}

fn open_queue(path: Option<PathBuf>) -> Result<Queue> {
    Queue::open(path.map(Ok).unwrap_or_else(Queue::default_path)?)
}

/// What to do once the command has finished.
#[derive(Default)]
struct Exit {
    failed: bool,
    pause: bool,
}
impl From<ConsoleResult> for Exit {
    fn from(result: ConsoleResult) -> Self {
        // The dashboard waits for the user itself; plain output in a visible
        // console window would otherwise vanish as soon as processing ends.
        let visible = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        Self {
            failed: result.failed,
            pause: result.needs_pause && visible,
        }
    }
}

/// Queue recordings, then process the queue (or follow the process already
/// doing so), showing a dashboard in a terminal and plain logs otherwise.
fn queue_and_watch(
    inputs: &[PathBuf],
    settings: &pipeline::Settings,
    queue_dir: Option<PathBuf>,
    background: bool,
) -> Result<Exit> {
    let queue = open_queue(queue_dir)?;
    let ids = queue.submit(inputs, settings)?;
    for id in &ids {
        println!("Queued {id}");
    }
    if background {
        queue.start_background(settings.jobs)?;
        return Ok(Exit::default());
    }
    Ok(queue
        .console(settings.jobs, &ids, &mut std::io::stderr())?
        .map(Exit::from)
        .unwrap_or_default())
}

/// Cluster recordings in this process, several at once, without the queue.
fn cluster_here(inputs: Vec<PathBuf>, settings: pipeline::Settings) -> Result<Exit> {
    let mut unique = VecDeque::new();
    for input in inputs {
        let input = std::fs::canonicalize(&input).unwrap_or(input);
        if !unique.contains(&input) {
            unique.push_back(input);
        }
    }
    let workers = unique.len().min(settings.jobs);
    let pending = Mutex::new(unique);
    let failed = Mutex::new(false);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while let Some(input) = pending.lock().unwrap().pop_front() {
                    let result =
                        pipeline::cluster_recording(&input, &settings, &mut std::io::stderr());
                    if let Err(error) = result {
                        eprintln!("ltxk: {}: {error}", input.display());
                        *failed.lock().unwrap() = true;
                    }
                }
            });
        }
    });
    Ok(Exit {
        failed: failed.into_inner().unwrap(),
        pause: false,
    })
}

fn status(queue_dir: Option<PathBuf>, json: bool) -> Result<Exit> {
    let queue = open_queue(queue_dir)?;
    let records = queue.records()?;
    if json {
        serde_json::to_writer_pretty(std::io::stdout(), &records)
            .map_err(|error| Error::Message(error.to_string()))?;
        println!();
        return Ok(Exit::default());
    }
    println!("Queue: {}", queue.root.display());
    for record in records {
        println!(
            "{:?}: {} [{}]",
            record.phase,
            record.input.display(),
            record.id
        );
        if let Some(error) = record.error {
            println!("  {error}");
        }
        let log = queue.root.join("logs").join(format!("{}.log", record.id));
        println!("  Log: {}", log.display());
    }
    Ok(Exit::default())
}

fn resume(jobs: Option<JobCount>, queue_dir: Option<PathBuf>, headless: bool) -> Result<Exit> {
    let queue = open_queue(queue_dir)?;
    let jobs = match jobs {
        Some(jobs) => jobs.0,
        None => {
            let settings = pipeline::Settings::load(None)?;
            settings.validate()?;
            settings.jobs
        }
    };
    if headless {
        let failed = queue.drain(jobs, &mut std::io::stderr())?;
        return Ok(Exit {
            failed,
            pause: false,
        });
    }
    Ok(queue
        .console(jobs, &[], &mut std::io::stderr())?
        .map(Exit::from)
        .unwrap_or_default())
}

fn run(cli: Cli) -> Result<Exit> {
    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut Cli::command(), "ltxk", &mut std::io::stdout());
        return Ok(Exit::default());
    }
    match cli.command {
        Some(Command::Enqueue { request, queue_dir }) => {
            let inputs: Vec<PathBuf> = serde_json::from_reader(std::fs::File::open(request)?)
                .map_err(|error| Error::Message(format!("Explorer selection: {error}")))?;
            if inputs.is_empty() {
                return Err(Error::Invalid("Select at least one recording"));
            }
            for input in &inputs {
                if !input.is_absolute() || !pipeline::is_recording_name(input) {
                    return Err(Error::Message(format!(
                        "Not a tetrode recording: {}",
                        input.display()
                    )));
                }
            }
            let settings = ClusterOptions::default().resolve()?;
            let ids = open_queue(queue_dir)?.submit(&inputs, &settings)?;
            println!("Queued {} recordings", ids.len());
            Ok(Exit::default())
        }
        Some(Command::Cluster {
            inputs,
            options,
            mode,
        }) => {
            let settings = options.resolve()?;
            if mode.no_queue {
                cluster_here(inputs, settings)
            } else {
                queue_and_watch(&inputs, &settings, mode.queue_dir, mode.background)
            }
        }
        Some(Command::Resume {
            jobs,
            queue_dir,
            headless,
        }) => resume(jobs, queue_dir, headless),
        Some(Command::Status { json, queue_dir }) => status(queue_dir, json),
        None if cli.inputs.is_empty() => resume(None, None, false),
        None => {
            let settings = ClusterOptions::default().resolve()?;
            queue_and_watch(&cli.inputs, &settings, None, false)
        }
    }
}

fn pause_console() {
    if std::io::stdin().is_terminal() {
        eprintln!("\nPress Enter to close this window.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

fn main() {
    let cli = Cli::parse();
    // A drag-and-drop or double-click opens a console window that closes on
    // exit, so keep errors on screen until the user has read them.
    let launched_from_desktop = cli.command.is_none() && cli.completions.is_none();
    match run(cli) {
        Ok(exit) => {
            if exit.pause {
                pause_console();
            }
            if exit.failed {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("ltxk: {error}");
            if launched_from_desktop {
                pause_console();
            }
            std::process::exit(1);
        }
    }
}
