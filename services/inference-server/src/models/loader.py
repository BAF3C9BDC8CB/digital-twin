"""Model download with aria2c acceleration, fallback to huggingface_hub."""

import logging
import os
import shutil
import subprocess

logger = logging.getLogger("dt-inference.models")

MODEL_CACHE_DIR = os.environ.get(
    "INFERENCE_CACHE_DIR",
    os.path.expanduser("~/.cache/digital-twin/models"),
)


def ensure_downloaded(model_name: str) -> str:
    """Download model with aria2c for speed, fallback to huggingface_hub.

    Returns the snapshot directory path.
    """
    from huggingface_hub import HfApi

    cache_dir = os.path.expanduser("~/.cache/huggingface/hub")
    org_repo = model_name.replace("/", "--")
    model_dir = os.path.join(cache_dir, f"models--{org_repo}")

    # 1. Get repo info from HF API
    api = HfApi()
    try:
        info = api.repo_info(model_name, files_metadata=False)
        sha = info.sha
    except Exception:
        from huggingface_hub import snapshot_download
        return snapshot_download(
            repo_id=model_name, cache_dir=cache_dir,
            resume_download=True,
            ignore_patterns=["*.h5", "*.ot", "*.msgpack"],
        )

    snapshot_dir = os.path.join(model_dir, "snapshots", sha)
    refs_dir = os.path.join(model_dir, "refs")

    # 2. Already cached? skip
    if os.path.isdir(snapshot_dir) and os.listdir(snapshot_dir):
        logger.info("Model %s already cached at %s", model_name, snapshot_dir)
        os.makedirs(refs_dir, exist_ok=True)
        with open(os.path.join(refs_dir, "main"), "w") as f:
            f.write(sha)
        return snapshot_dir

    os.makedirs(snapshot_dir, exist_ok=True)
    os.makedirs(refs_dir, exist_ok=True)

    # 3. Download with aria2c if available
    base_url = f"https://huggingface.co/{model_name}/resolve/main"
    files = [
        s.rfilename for s in info.siblings
        if not any(s.rfilename.endswith(pat) for pat in (".h5", ".ot", ".msgpack"))
    ]

    logger.info("Downloading %s (%d files) with aria2c...", model_name, len(files))

    if shutil.which("aria2c"):
        url_list = []
        for fname in files:
            url_list.append(f"{base_url}/{fname}")
            url_list.append(f"  out={fname}")
        input_file = os.path.join(snapshot_dir, ".aria2_input.txt")
        with open(input_file, "w") as f:
            f.write("\n".join(url_list))

        subprocess.run([
            "aria2c", f"--input-file={input_file}",
            f"--dir={snapshot_dir}",
            "--max-concurrent-downloads=5",
            "--max-connection-per-server=16", "--split=16",
            "--continue=true", "--console-log-level=warn",
        ], check=True)
    else:
        from huggingface_hub import snapshot_download
        return snapshot_download(
            repo_id=model_name, cache_dir=cache_dir,
            resume_download=True,
            ignore_patterns=["*.h5", "*.ot", "*.msgpack"],
        )

    # 4. Write refs/main
    with open(os.path.join(refs_dir, "main"), "w") as f:
        f.write(sha)

    logger.info("Model %s downloaded to %s", model_name, snapshot_dir)
    return snapshot_dir
