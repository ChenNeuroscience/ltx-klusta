"""Check the command line: help, completions, validation, and every --dump point."""
import argparse
import math
from pathlib import Path
import tempfile

from common import command, read_npy, recording, require


def check_cli(exe):
    for flag in ("--help", "-h", "--version", "-V", "cluster --help"):
        result = command(exe, *flag.split())
        require(result.returncode == 0 and result.stdout, f"CLI {flag} failed: {result.stderr}")
    for shell in ("bash", "zsh", "fish", "powershell", "elvish"):
        result = command(exe, "--completions", shell)
        require(result.returncode == 0 and "ltxk" in result.stdout, result.stderr)
    for flags in (("--block-spikes", "0"), ("--block-spikes", "65537"), ("--block-spikes", "oops"),
                  ("--blok-spikes", "7"), ("--dump", "everything"), ("--features", "4pc"),
                  ("--no-queue", "--background"), ("--no-queue", "--queue-dir", "q")):
        result = command(exe, "cluster", "x.1", *flags)
        require(result.returncode == 2 and "error:" in result.stderr, f"CLI accepted {flags}")
    result = command(exe, "cluster", "x.1", "--no-queue", "--dump-only")
    require(result.returncode != 0 and "--dump" in result.stderr, "--dump-only without --dump accepted")


def check_dumps(exe):
    with tempfile.TemporaryDirectory(prefix="ltxk dumps ") as directory:
        root = Path(directory)
        source = root / "wave λ.4"
        spikes = 1201
        recording(source, spikes, active_channels=2)
        flags = ("--no-queue", "--min-clusters", "2", "--max-clusters", "3", "--target-spikes", "250")
        result = command(exe, "cluster", source, *flags, "--dump", "pca,features,subsets,models,labels")
        require(result.returncode == 0, result.stderr)
        dump = root / "wave λ.dump.4"
        def shape(name):
            return read_npy(dump / f"{name}.npy")[0]
        require(shape("pca_channels") == (2,) and read_npy(dump / "pca_channels.npy")[1] == [0, 1],
                "Wrong PCA channel dump")
        require(shape("pca_means") == (2, 50) and shape("pca_components") == (2, 3, 50), "Wrong PCA dump")
        require(shape("features") == (spikes, 6), "Wrong feature dump")
        require(shape("feature_min") == shape("feature_range") == (6,), "Wrong bounds dump")
        for trial in (2, 3):
            require(shape(f"trial_{trial}_subset") == (300,), "Wrong subset dump")
            subset = read_npy(dump / f"trial_{trial}_subset.npy")[1]
            require(all(4 * i <= s < 4 * i + 4 for i, s in enumerate(subset)), "Subset is not stratified")
            (k,), weights = read_npy(dump / f"trial_{trial}_weights.npy")
            require(shape(f"trial_{trial}_means") == (k, 6), "Wrong means dump")
            require(shape(f"trial_{trial}_covariances") == (k, 6, 6), "Wrong covariance dump")
            _, covariances = read_npy(dump / f"trial_{trial}_covariances.npy")
            for c in range(1, k):
                matrix = covariances[c * 36:(c + 1) * 36]
                if not math.isnan(matrix[0]):
                    require(all(matrix[i * 6 + j] == matrix[j * 6 + i] for i in range(6) for j in range(6)),
                            "Covariance dump is not symmetric")
            labels = read_npy(dump / f"trial_{trial}_labels.npy")[1]
            require(len(labels) == spikes and max(labels) < k, "Labels do not match the model dump")
            require(weights[0] > 0 and all(w >= 0 for w in weights), "Invalid component weights")
        require(source.with_suffix(".clu.4").is_file(), "A dump run should still write .clu")
        # --dump-only stops after the requested data and leaves .clu alone.
        clu = source.with_suffix(".clu.4")
        clu.write_bytes(b"existing")
        result = command(exe, "cluster", source, *flags, "--dump", "features", "--dump-only",
                         "--dump-dir", root / "elsewhere")
        require(result.returncode == 0 and "clustering skipped" in result.stderr, result.stderr)
        require((root / "elsewhere" / source.name / "features.npy").is_file(), "--dump-dir ignored")
        require(not (root / "elsewhere" / source.name / "trial_2_labels.npy").exists(), "Unrequested dump")
        require(clu.read_bytes() == b"existing", "--dump-only replaced .clu")
    print("CLI validation, completions, and all dump points passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True, type=Path)
    exe = parser.parse_args().exe.resolve()
    check_cli(exe)
    check_dumps(exe)
