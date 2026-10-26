use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use ltx_klusta::{pipeline, queue::Queue, Backend, Error, Features, Result, MAX_BLOCK_SPIKES};
use std::collections::VecDeque;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Mutex;

/// Cluster legacy tetrode recordings with PCA and classic KlustaKwik clustering.
#[derive(Debug, Parser)]
#[command(
    name = "ltx-klusta",
    bin_name = "ltxk",
    version,
    arg_required_else_help = true,
    args_conflicts_with_subcommands = true,
    after_help = "EXAMPLES:\n  ltxk cluster recording.1\n  ltxk cluster recording.1 recording.2 --backend auto\n  ltxk enqueue recording.1 --background\n  ltxk status\n  ltxk features recording.1 temp/recording temp/count.txt 3PC\n  ltxk --completions powershell\n\nDropping recordings onto the executable also clusters them with the default configuration."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Recordings to cluster using ltxk.json or defaults (drag-and-drop)
    #[arg(value_hint = clap::ValueHint::FilePath)]
    inputs: Vec<PathBuf>,
    /// Write shell completion definitions and exit
    #[arg(long, value_enum, exclusive = true)]
    completions: Option<Shell>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Extract features, fit clusters, and write .clu.N beside each recording
    Cluster {
        #[arg(required = true, num_args = 1.., value_hint = clap::ValueHint::FilePath)]
        inputs: Vec<PathBuf>,
        #[command(flatten)]
        options: ClusterOptions,
    },
    /// Queue recordings across independent invocations (four concurrent jobs by default)
    Enqueue {
        #[arg(required = true, num_args = 1.., value_hint = clap::ValueHint::FilePath)]
        inputs: Vec<PathBuf>,
        #[command(flatten)]
        options: ClusterOptions,
        /// Start a detached coordinator and return immediately
        #[arg(long)]
        background: bool,
        /// Show a shared live dashboard (plain logs without a terminal); keep it open at completion
        #[arg(long, conflicts_with = "background")]
        console: bool,
        /// Override the per-user queue directory
        #[arg(long)]
        queue_dir: Option<PathBuf>,
    },
    /// Resume queued or interrupted jobs after a coordinator exits
    Resume {
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..=16))]
        jobs: u32,
        /// Show a live dashboard (plain logs without a terminal); keep it open at completion
        #[arg(long)]
        console: bool,
        #[arg(long)]
        queue_dir: Option<PathBuf>,
    },
    /// Show pending, running, completed, and failed jobs, with log locations
    Status {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        queue_dir: Option<PathBuf>,
    },
    /// Extract a .fet.N file and spike count without clustering
    Features {
        /// Tetrode recording with a numeric extension
        input: PathBuf,
        /// Output path without .fet.N
        output_base: PathBuf,
        /// File in which to write the spike count
        spike_count_file: PathBuf,
        /// Comma-separated features: 3PC, 3PC,A,VT, or A,VT
        #[arg(value_parser = Features::parse)]
        features: Features,
        #[command(flatten)]
        numerical: Numerical,
    },
}

#[derive(Debug, Default, Args)]
struct Numerical {
    /// PCA backend (default: nalgebra; a library path implies lapack)
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    /// Optional LP64 BLAS/LAPACK provider loaded at runtime
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    lapack_library: Option<PathBuf>,
    /// Maximum waveform block size (default: 4096 spikes)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=MAX_BLOCK_SPIKES as i64))]
    block_spikes: Option<u32>,
}
impl Numerical {
    fn apply(self, settings: &mut pipeline::Settings) {
        if let Some(backend) = self.backend {
            settings.backend = backend;
        }
        if let Some(library) = self.lapack_library {
            if self.backend.is_none() {
                settings.backend = Backend::Lapack;
            }
            settings.lapack_library = Some(library);
        }
        if let Some(block) = self.block_spikes {
            settings.block_spikes = block as usize;
        }
        if settings.backend == Backend::Nalgebra && settings.lapack_library.is_some() {
            exit_cli_error(Cli::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "--lapack-library cannot be used with --backend nalgebra",
            ));
        }
    }
}

#[derive(Debug, Default, Args)]
struct ClusterOptions {
    /// JSON settings file (default: ltxk.json beside the executable, if present)
    #[arg(long)]
    config: Option<PathBuf>,
    /// Features supplied to clustering (default: 3PC)
    #[arg(long, value_parser = parse_features)]
    features: Option<String>,
    /// Smallest initial cluster count, including noise (default: 20)
    #[arg(long, value_parser = clap::value_parser!(u32).range(2..100))]
    min_clusters: Option<u32>,
    /// Largest initial cluster count, including noise (default: 30)
    #[arg(long, value_parser = clap::value_parser!(u32).range(2..100))]
    max_clusters: Option<u32>,
    /// Fitting target, with the historical floor(N/target) stride (default: 25000)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=25_000))]
    target_spikes: Option<u32>,
    /// Windows-compatible random seed (default: 1)
    #[arg(long)]
    seed: Option<u32>,
    /// Maximum CEM iterations per fit (default: 10000)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100_000))]
    max_iterations: Option<u32>,
    /// Maximum simultaneous recordings (default: 4)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=16))]
    jobs: Option<u32>,
    /// Replace existing .clu.N files (otherwise skip them)
    #[arg(long)]
    force: bool,
    /// Scratch directory on a disk with room for temporary features
    #[arg(long)]
    work_dir: Option<PathBuf>,
    #[command(flatten)]
    numerical: Numerical,
}

fn parse_features(text: &str) -> std::result::Result<String, String> {
    Features::parse(text)
        .map(|_| text.to_owned())
        .map_err(|error| error.to_string())
}
impl ClusterOptions {
    fn resolve(self) -> Result<pipeline::Settings> {
        let mut settings = pipeline::Settings::load(self.config.as_deref())?;
        if let Some(features) = self.features {
            settings.features = features;
        }
        if let Some(value) = self.min_clusters {
            settings.cluster.min_clusters = value as usize;
        }
        if let Some(value) = self.max_clusters {
            settings.cluster.max_clusters = value as usize;
        }
        if let Some(value) = self.target_spikes {
            settings.cluster.target_spikes = value as usize;
        }
        if let Some(value) = self.seed {
            settings.cluster.seed = value;
        }
        if let Some(value) = self.max_iterations {
            settings.cluster.max_iterations = value as usize;
        }
        if let Some(value) = self.jobs {
            settings.jobs = value as usize;
        }
        if self.force {
            settings.force = true;
        }
        if let Some(path) = self.work_dir {
            settings.work_dir = Some(path);
        }
        self.numerical.apply(&mut settings);
        for path in [&mut settings.lapack_library, &mut settings.work_dir]
            .into_iter()
            .flatten()
        {
            if path.is_relative() {
                *path = std::env::current_dir()?.join(&path);
            }
        }
        settings.validate()?;
        Ok(settings)
    }
}

fn queue(path: Option<PathBuf>) -> Result<Queue> {
    Queue::open(path.map(Ok).unwrap_or_else(Queue::default_path)?)
}

fn cluster_many(inputs: Vec<PathBuf>, settings: pipeline::Settings) -> Result<()> {
    let mut unique = VecDeque::new();
    for input in inputs {
        let input = std::fs::canonicalize(&input).unwrap_or(input);
        if !unique.contains(&input) {
            unique.push_back(input);
        }
    }
    let count = unique.len().min(settings.jobs);
    let pending = Mutex::new(unique);
    let failed = std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..count {
            workers.push(scope.spawn(|| {
                let mut failed = false;
                loop {
                    let Some(input) = pending.lock().unwrap().pop_front() else {
                        break;
                    };
                    if let Err(error) =
                        pipeline::cluster_recording(&input, &settings, &mut std::io::stderr())
                    {
                        eprintln!("ltxk: {}: {error}", input.display());
                        failed = true;
                    }
                }
                failed
            }));
        }
        let mut failed = false;
        for worker in workers {
            failed |= worker.join().unwrap_or(true);
        }
        failed
    });
    if failed {
        Err(Error::Invalid(
            "Some recordings failed; see the errors above",
        ))
    } else {
        Ok(())
    }
}

#[derive(Default)]
struct Exit {
    code: i32,
    pause: bool,
}

fn run(cli: Cli) -> Result<Exit> {
    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut Cli::command(), "ltxk", &mut std::io::stdout());
        return Ok(Exit::default());
    }
    match cli.command {
        Some(Command::Cluster { inputs, options }) => {
            cluster_many(inputs, options.resolve()?).map(|_| Exit::default())
        }
        Some(Command::Enqueue {
            inputs,
            options,
            background,
            console,
            queue_dir,
        }) => {
            let settings = options.resolve()?;
            let queue = queue(queue_dir)?;
            let ids = queue.submit(&inputs, &settings)?;
            for id in &ids {
                println!("Queued {id}");
            }
            if console {
                return console_result(queue.console(
                    settings.jobs,
                    &ids,
                    &mut std::io::stderr(),
                )?);
            }
            if background {
                queue.start_background(settings.jobs)?;
            } else if queue.drain(settings.jobs, &mut std::io::stderr())? {
                return Err(Error::Invalid(
                    "Some queued jobs failed; use ltxk status for details",
                ));
            }
            Ok(Exit::default())
        }
        Some(Command::Resume {
            jobs,
            console,
            queue_dir,
        }) => {
            let queue = queue(queue_dir)?;
            if console {
                return console_result(queue.console(
                    jobs as usize,
                    &[],
                    &mut std::io::stderr(),
                )?);
            }
            if queue.drain(jobs as usize, &mut std::io::stderr())? {
                Err(Error::Invalid(
                    "Some queued jobs failed; use ltxk status for details",
                ))
            } else {
                Ok(Exit::default())
            }
        }
        Some(Command::Status { json, queue_dir }) => {
            let queue = queue(queue_dir)?;
            let records = queue.records()?;
            if json {
                serde_json::to_writer_pretty(std::io::stdout(), &records)
                    .map_err(|error| Error::Message(error.to_string()))?;
                println!();
            } else {
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
                    println!(
                        "  Log: {}",
                        queue
                            .root
                            .join("logs")
                            .join(format!("{}.log", record.id))
                            .display()
                    );
                }
            }
            Ok(Exit::default())
        }
        Some(Command::Features {
            input,
            output_base,
            spike_count_file,
            features,
            numerical,
        }) => {
            let mut settings = pipeline::Settings::default();
            numerical.apply(&mut settings);
            let output = ltx_klusta::extract(&ltx_klusta::Options {
                input,
                output_base,
                info: spike_count_file,
                features,
                backend: settings.backend,
                lapack_library: settings.lapack_library,
                block_spikes: settings.block_spikes,
            })?;
            println!("Features written to {}", output.display());
            Ok(Exit::default())
        }
        None => {
            cluster_many(cli.inputs, ClusterOptions::default().resolve()?).map(|_| Exit::default())
        }
    }
}

fn console_result(result: Option<ltx_klusta::queue::ConsoleResult>) -> Result<Exit> {
    match result {
        None => Ok(Exit::default()),
        Some(result) => Ok(Exit {
            code: i32::from(result.failed),
            pause: result.needs_pause,
        }),
    }
}

fn pause_console() {
    if std::io::stdin().is_terminal() {
        eprintln!("\nPress Enter to close this console session.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

fn exit_cli_error(error: clap::Error) -> ! {
    let _ = error.print();
    if error.use_stderr() && std::env::args_os().any(|arg| arg == "--console") {
        pause_console();
    }
    std::process::exit(error.exit_code());
}

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let console = args.iter().any(|arg| arg == "--console");
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => exit_cli_error(error),
    };
    match run(cli) {
        Ok(exit) => {
            if exit.pause {
                pause_console();
            }
            if exit.code != 0 {
                std::process::exit(exit.code);
            }
        }
        Err(error) => {
            eprintln!("ltxk: {error}");
            if console {
                pause_console();
            }
            std::process::exit(1);
        }
    }
}
