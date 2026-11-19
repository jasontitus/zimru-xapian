#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise the audited Ubuntu image's native startup and extraction contracts."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
import os
from pathlib import Path
import resource
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def run(image, args, cwd, env, *, limit_output_file=False):
    def limits():
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        resource.setrlimit(resource.RLIMIT_FSIZE, (1024 * 1024, 1024 * 1024))
    return subprocess.run([image, *args], cwd=cwd, env=env, capture_output=True,
                          text=True, timeout=60, preexec_fn=limits if limit_output_file else None)


def check_cancellation(image, root, env, area, kept):
    def withdraw(proc):
        if proc.poll() is None:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(10)

    def track_child(proc, cleanup):
        children = Path(f"/proc/{proc.pid}/task/{proc.pid}/children").read_text().split()
        assert len(children) == 1
        fd = os.pidfd_open(int(children[0]))
        cleanup.callback(os.close, fd)

        def finish():
            if not select.select([fd], [], [], 0)[0]:
                try:
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                assert select.select([fd], [], [], 8)[0], "fixture child remained alive"
        cleanup.callback(finish)
        return fd

    with ExitStack() as cleanup:
        procs = []
        for index in range(3):
            profile = root / f"profile-{index}"
            proc = subprocess.Popen([image, "--headless", "--data-dir", profile, "--port", "0"],
                                    cwd=root, env={**env, "APPIMAGE_EXTRACT_AND_RUN": "1"},
                                    start_new_session=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            cleanup.callback(withdraw, proc)
            cleanup.callback(proc.stdout.close)
            assert select.select([proc.stdout], [], [], 30)[0], "headless startup timed out"
            assert proc.stdout.readline(512).startswith(b"KIWIX_READY ")
            procs.append((proc, track_child(proc, cleanup)))

        for index, event in enumerate((signal.SIGINT, signal.SIGTERM, signal.SIGTERM)):
            proc, fd = procs[index]
            if index == 2:
                # This owned child cannot handle TERM while stopped. The supervisor
                # must still reap it and withdraw its private tree before returning.
                signal.pidfd_send_signal(fd, signal.SIGSTOP)
            proc.send_signal(event)
            assert proc.wait(12) == 128 + event
            assert select.select([fd], [], [], 1)[0], "owned reader survived supervisor cancellation"
            assert all(other.poll() is None for other, _ in procs[index + 1:])
        assert set(area.iterdir()) == kept

    # A background caller can deliberately block or ignore INT. Its supervisor
    # must leave that decision intact, including after the termination deadline.
    with ExitStack() as cleanup:
        waiting = []
        for mode in ("blocked", "ignored"):
            def inherited_interrupt(mode=mode):
                if mode == "blocked":
                    signal.pthread_sigmask(signal.SIG_BLOCK, [signal.SIGINT])
                else:
                    signal.signal(signal.SIGINT, signal.SIG_IGN)
            proc = subprocess.Popen([image, "--headless", "--data-dir", root / mode, "--port", "0"],
                                    cwd=root, env={**env, "APPIMAGE_EXTRACT_AND_RUN": "1"},
                                    start_new_session=True, preexec_fn=inherited_interrupt,
                                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            cleanup.callback(withdraw, proc)
            cleanup.callback(proc.stdout.close)
            assert select.select([proc.stdout], [], [], 30)[0]
            assert proc.stdout.readline(512).startswith(b"KIWIX_READY ")
            track_child(proc, cleanup)
            proc.send_signal(signal.SIGINT)
            waiting.append(proc)
        time.sleep(7)
        for proc in waiting:
            assert proc.poll() is None, "inherited interrupt policy changed"
            proc.terminate()
            assert proc.wait(12) == 128 + signal.SIGTERM
        assert set(area.iterdir()) == kept


def exercise(original):
    with tempfile.TemporaryDirectory(prefix="kiwix-loader-") as tmp:
        root = Path(tmp)
        image = root / "Kiwix test.AppImage"
        shutil.copy2(original, image)
        env = {k: v for k, v in os.environ.items()
               if k not in ("APPIMAGE_EXTRACT_AND_RUN", "NO_CLEANUP", "TARGET_APPIMAGE")}
        area = root / "temporary"; area.mkdir(mode=0o700)
        env.update(TMPDIR=str(area), HOME=str(root))
        # Common TMPDIR, different launch option forms and separate live trees.
        with ThreadPoolExecutor(max_workers=2) as executor:
            env_launch = executor.submit(run, image, ["--version"], root,
                                         {**env, "APPIMAGE_EXTRACT_AND_RUN": "1", "NO_CLEANUP": "1"})
            flag_launch = executor.submit(run, image, ["--appimage-extract-and-run", "--version"], root,
                                          {**env, "NO_CLEANUP": "1"})
            first, second = env_launch.result(), flag_launch.result()
        assert first.returncode == second.returncode == 0, (first.stderr, second.stderr)
        assert first.stdout.strip().startswith("Kiwix Rust ") and first.stdout == second.stdout
        kept = set(area.iterdir())
        assert len(kept) == 2
        for tree in kept:
            assert tree.stat().st_mode & 0o777 == 0o700
            assert (tree / "AppDir/usr/bin/kiwix-workbench").is_file()
        insufficient = run(image, ["--version"], root, {**env, "APPIMAGE_EXTRACT_AND_RUN": "1"},
                           limit_output_file=True)
        assert insufficient.returncode and not insufficient.stdout.strip().startswith("Kiwix Rust ")
        assert set(area.iterdir()) == kept, "failed extraction must withdraw its own partial tree"

        assert run(image, ["--appimage-offset"], root, {**env, "TMPDIR": str(root / "missing")}).stdout.strip().isdigit()
        unusable = run(image, ["--version"], root, {**env, "TMPDIR": str(root / "missing"), "APPIMAGE_EXTRACT_AND_RUN": "1"})
        assert unusable.returncode and "TMPDIR" in unusable.stderr

        if not os.path.exists("/dev/fuse"):
            unavailable = run(image, ["--appimage-mount"], root, env)
            assert unavailable.returncode and "FUSE mount failed" in unavailable.stderr
            assert not any(line.startswith("/") for line in unavailable.stdout.splitlines())
            assert set(area.iterdir()) == kept, "failed mount must withdraw its empty mountpoint"

        extracted = run(image, ["--appimage-extract", "AppRun"], root, env)
        assert extracted.returncode == 0 and (root / "squashfs-root/AppRun").is_file(), extracted.stderr
        marker = root / "squashfs-root/selected-document"; marker.write_text("keep")
        repeat = run(image, ["--appimage-extract"], root, env)
        assert repeat.returncode and marker.read_text() == "keep"

        sidecar = Path(str(image) + ".config"); sidecar.write_text("keep")
        invalid = run(image, ["--appimage-portable-config"], root, env)
        assert invalid.returncode and sidecar.read_text() == "keep"
        sidecar.unlink()
        assert run(image, ["--appimage-portable-config"], root, env).returncode == 0
        assert sidecar.is_dir() and sidecar.stat().st_mode & 0o777 == 0o700
        final = run(image, ["--appimage-extract-and-run", "--version"], root, env)
        assert final.returncode == 0 and final.stdout == first.stdout
        assert set(area.iterdir()) == kept
        check_cancellation(image, root, env, area, kept)


if __name__ == "__main__":
    exercise(Path(sys.argv[1]).resolve(strict=True))
    print("AppImage startup, ownership, cleanup, options and failure checks passed.")
