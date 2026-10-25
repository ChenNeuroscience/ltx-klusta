use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use prekk::{Backend, Features, Options, DEFAULT_BLOCK_SPIKES, MAX_BLOCK_SPIKES};
use std::path::PathBuf;

/// Extract KlustaKwik features from Axona tetrode recordings using bounded memory.
#[derive(Debug, Parser)]
#[command(
    version,
    after_help = "EXAMPLES:\n  prekk recording.1 temp/recording temp/count.txt 3PC\n  prekk recording.1 temp/recording temp/count.txt 3PC,A,VT --backend auto\n  prekk recording.1 temp/recording temp/count.txt 3PC --lapack-library PATH\n  prekk --completions powershell\n\nOutput columns: all active channels' PCs, then amplitudes, then VT."
)]
struct Cli {
    /// Input tetrode recording, including its numeric extension
    #[arg(required = true)]
    input: Option<PathBuf>,
    /// Output path without the .fet.N suffix
    #[arg(required = true)]
    output_base: Option<PathBuf>,
    /// File in which to write the total spike count
    #[arg(required = true)]
    spike_count_file: Option<PathBuf>,
    /// Comma-separated features, e.g. 3PC, 3PC,A,VT, or A,VT
    #[arg(required = true, value_parser = Features::parse)]
    features: Option<Features>,
    /// Numerical backend (default: nalgebra; a library path selects lapack)
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    /// Optional LP64 BLAS/LAPACK library to load at runtime
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    lapack_library: Option<PathBuf>,
    /// Maximum spikes held in one processing block
    #[arg(long, default_value_t = DEFAULT_BLOCK_SPIKES as u32,
          value_parser = clap::value_parser!(u32).range(1..=MAX_BLOCK_SPIKES as i64))]
    block_spikes: u32,
    /// Write shell completion definitions to standard output and exit
    #[arg(long, value_enum, exclusive = true)]
    completions: Option<Shell>,
}

fn main() {
    let cli = Cli::parse();
    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut Cli::command(), "prekk", &mut std::io::stdout());
        return;
    }
    // clap enforces all four positionals unless completions were requested.
    let backend = cli.backend.unwrap_or(if cli.lapack_library.is_some() {
        Backend::Lapack
    } else {
        Backend::Nalgebra
    });
    if backend == Backend::Nalgebra && cli.lapack_library.is_some() {
        Cli::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                "--lapack-library cannot be used with --backend nalgebra",
            )
            .exit();
    }
    let options = Options {
        input: cli.input.unwrap(),
        output_base: cli.output_base.unwrap(),
        info: cli.spike_count_file.unwrap(),
        features: cli.features.unwrap(),
        backend,
        lapack_library: cli.lapack_library,
        block_spikes: cli.block_spikes as usize,
    };
    match prekk::extract(&options) {
        Ok(output) => println!("Features written to {}", output.display()),
        Err(error) => {
            eprintln!("prekk: {error}");
            std::process::exit(1);
        }
    }
}
