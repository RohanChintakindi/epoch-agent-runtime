#!/usr/bin/env python3
"""Bounded Linux fork/COW matrix probe with an independent memory preflight."""

import json
import os
import resource
import sys
import time

MIB = 1024 * 1024
GIB = 1024 * MIB
MAX_ALLOCATION = 2 * GIB
MAX_FANOUT = 8
CHILD_OVERHEAD = 32 * MIB
RUNNER_OVERHEAD = 64 * MIB


def memory_bytes():
    values = {}
    with open("/proc/self/smaps_rollup", "r", encoding="ascii") as stream:
        for line in stream:
            fields = line.split()
            if len(fields) >= 2 and fields[0] in ("Rss:", "Pss:"):
                values[fields[0][:-1].lower()] = int(fields[1]) * 1024
    if "rss" not in values or "pss" not in values:
        raise RuntimeError("smaps_rollup omitted RSS or PSS")
    return values


def available_memory_bytes():
    with open("/proc/meminfo", "r", encoding="ascii") as stream:
        for line in stream:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) * 1024
    raise RuntimeError("MemAvailable is absent from /proc/meminfo")


def estimated_peak(allocation, fanout, dirty_basis_points):
    dirty = (allocation * dirty_basis_points + 9999) // 10000
    return (
        allocation * 2
        + dirty * fanout
        + CHILD_OVERHEAD * fanout
        + RUNNER_OVERHEAD
    )


def child_probe(memory, dirty_pages, page_size, output_fd, release_fd):
    before = resource.getrusage(resource.RUSAGE_SELF)
    started = time.perf_counter_ns()
    for page in range(dirty_pages):
        offset = page * page_size
        memory[offset] = (memory[offset] + 1) & 0xFF
    usage = resource.getrusage(resource.RUSAGE_SELF)
    resident = memory_bytes()
    result = {
        "minor_faults": usage.ru_minflt - before.ru_minflt,
        "major_faults": usage.ru_majflt - before.ru_majflt,
        "rss_bytes": resident["rss"],
        "pss_bytes": resident["pss"],
        "dirty_ns": time.perf_counter_ns() - started,
    }
    os.write(output_fd, (json.dumps(result, sort_keys=True) + "\n").encode("ascii"))
    os.close(output_fd)
    os.read(release_fd, 1)
    os.close(release_fd)
    os._exit(0)


def read_line(fd):
    chunks = []
    while True:
        chunk = os.read(fd, 4096)
        if not chunk:
            break
        chunks.append(chunk)
        if b"\n" in chunk:
            break
    os.close(fd)
    return json.loads(b"".join(chunks).decode("ascii"))


def run(allocation, fanout, dirty_basis_points, caller_budget):
    if not os.path.exists("/proc/self/smaps_rollup"):
        return {"status": "failed", "reason": "/proc/self/smaps_rollup is unavailable"}
    estimate = estimated_peak(allocation, fanout, dirty_basis_points)
    live_budget = min(caller_budget, available_memory_bytes() // 2)
    if estimate > live_budget:
        return {
            "status": "skipped",
            "reason": f"live estimate {estimate} exceeds live safety budget {live_budget}",
        }

    started = time.perf_counter_ns()
    page_size = os.sysconf("SC_PAGE_SIZE")
    rounded = ((allocation + page_size - 1) // page_size) * page_size
    allocation_started = time.perf_counter_ns()
    memory = bytearray(rounded)
    for offset in range(0, rounded, page_size):
        memory[offset] = 1
    allocation_ns = time.perf_counter_ns() - allocation_started

    full_copy_started = time.perf_counter_ns()
    for _ in range(fanout):
        control = bytearray(memory)
        if len(control) != rounded:
            raise RuntimeError("full-copy control length mismatch")
        del control
    full_copy_ns = time.perf_counter_ns() - full_copy_started

    page_count = rounded // page_size
    dirty_pages = (page_count * dirty_basis_points + 9999) // 10000
    processes = []
    fork_started = time.perf_counter_ns()
    for _ in range(fanout):
        output_read, output_write = os.pipe()
        release_read, release_write = os.pipe()
        pid = os.fork()
        if pid == 0:
            os.close(output_read)
            os.close(release_write)
            child_probe(memory, dirty_pages, page_size, output_write, release_read)
        os.close(output_write)
        os.close(release_read)
        processes.append((pid, output_read, release_write))
    fork_pause_ns = time.perf_counter_ns() - fork_started

    children = [read_line(output) for _, output, _ in processes]
    parent = memory_bytes()
    for _, _, release in processes:
        os.write(release, b"x")
        os.close(release)
    for pid, _, _ in processes:
        waited, status = os.waitpid(pid, 0)
        if waited != pid or status != 0:
            raise RuntimeError("COW child did not exit cleanly")

    return {
        "status": "succeeded",
        "runtime_ns": time.perf_counter_ns() - started,
        "allocation_ns": allocation_ns,
        "fork_pause_ns": fork_pause_ns,
        "dirty_ns_max": max(item["dirty_ns"] for item in children),
        "minor_faults": sum(item["minor_faults"] for item in children),
        "major_faults": sum(item["major_faults"] for item in children),
        "cow_rss_bytes": parent["rss"] + sum(item["rss_bytes"] for item in children),
        "cow_pss_bytes": parent["pss"] + sum(item["pss_bytes"] for item in children),
        "full_copy_bytes": rounded * fanout,
        "full_copy_ns": full_copy_ns,
    }


def main():
    if len(sys.argv) != 5:
        raise ValueError(
            "expected allocation_bytes fanout dirty_basis_points safety_budget_bytes"
        )
    allocation, fanout, dirty, budget = (int(value) for value in sys.argv[1:])
    if allocation < 4096 or allocation > MAX_ALLOCATION:
        raise ValueError("allocation is outside the 4KiB-2GiB bound")
    if fanout < 1 or fanout > MAX_FANOUT:
        raise ValueError("fanout is outside the 1-8 bound")
    if dirty < 0 or dirty > 10000:
        raise ValueError("dirty ratio is outside 0-10000 basis points")
    if budget <= 0:
        raise ValueError("safety budget must be positive")
    print(json.dumps(run(allocation, fanout, dirty, budget), sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(json.dumps({"status": "failed", "reason": str(error)}, sort_keys=True))
        sys.exit(1)
