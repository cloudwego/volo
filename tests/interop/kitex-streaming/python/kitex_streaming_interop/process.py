"""Bounded subprocess lifecycle shared by every interoperability profile."""
from __future__ import annotations

import json
import os
import queue
import signal
import subprocess
import threading
import time
from pathlib import Path


class ProcessFailure(RuntimeError):
    pass


def stop(process: subprocess.Popen, grace: float = 3.0) -> None:
    """Reap the process and its process group, including compiler children."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=grace)


def run(command: list[str], *, cwd: Path, timeout: float, log: Path,
        env: dict[str, str] | None = None) -> dict:
    log.parent.mkdir(parents=True, exist_ok=True)
    start = time.monotonic()
    with log.open("wb") as output:
        process = subprocess.Popen(command, cwd=cwd, env=env, stdout=output,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        timed_out = False
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            stop(process)
        except BaseException:
            stop(process)
            raise
    return {"command": command, "cwd": str(cwd), "returncode": process.returncode,
            "timed_out": timed_out, "duration_seconds": time.monotonic() - start,
            "log": str(log)}


class Server:
    """Readiness is an endpoint-generated JSON event, never a fixed sleep."""

    def __init__(self, command: list[str], *, cwd: Path, log: Path,
                 env: dict[str, str] | None = None, stdin: bool = False,
                 ready_event: str = 'ready', capture_events: bool = False):
        self.command, self.cwd, self.log, self.env = command, cwd, log, env
        self.stdin, self.ready_event, self.capture_events = stdin, ready_event, capture_events
        self.messages: queue.Queue = queue.Queue()
        self.events: queue.Queue = queue.Queue(maxsize=1)
        self.process = None

    def start(self, timeout: float = 30.0) -> dict:
        self.log.parent.mkdir(parents=True, exist_ok=True)
        self.process = subprocess.Popen(self.command, cwd=self.cwd, env=self.env,
                                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                        stdin=subprocess.PIPE if self.stdin else None,
                                        start_new_session=True)
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        try:
            event = self.events.get(timeout=timeout)
            if isinstance(event, Exception):
                raise event
            if self.process.poll() is not None:
                raise ProcessFailure(f"server exited during readiness: {self.log}")
            return event
        except BaseException:
            self.close()
            raise

    def _read(self) -> None:
        ready = False
        with self.log.open("wb") as output:
            for line in iter(self.process.stdout.readline, b""):
                output.write(line)
                output.flush()
                if (not ready or self.capture_events) and len(line) < 1_048_576:
                    try:
                        event = json.loads(line)
                    except (ValueError, UnicodeError):
                        continue
                    if self.capture_events and isinstance(event, dict):
                        self.messages.put(event)
                    if not ready and isinstance(event, dict) and event.get("event") == self.ready_event:
                        self.events.put(event)
                        ready = True
        if not ready:
            self.events.put(ProcessFailure(f"server exited before readiness: {self.log}"))

    def wait_event(self, name: str, timeout: float = 10.0) -> dict:
        if not self.capture_events:
            raise ProcessFailure('event capture was not enabled')
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ProcessFailure(f'no {name} event before deadline: {self.log}')
            try:
                event = self.messages.get(timeout=remaining)
            except queue.Empty as error:
                raise ProcessFailure(f'no {name} event before deadline: {self.log}') from error
            if event.get('event') == name:
                return event

    def send_line(self, line: str) -> None:
        if self.process is None or self.process.stdin is None:
            raise ProcessFailure('process does not have a controlled stdin')
        self.process.stdin.write((line + '\n').encode('utf-8'))
        self.process.stdin.flush()

    def close(self) -> None:
        if self.process is not None:
            if self.process.stdin is not None:
                self.process.stdin.close()
            stop(self.process)
            self.reader.join(timeout=5)
            self.process.stdout.close()
