#!/usr/bin/env python3
"""Small Linux-only fork/COW measurement helper. No network or external files are touched."""

import json
import os
import resource
import sys
import time

MAX_ALLOCATION = 256 * 1024 * 1024
MAX_CHILDREN = 16
MAX_TOTAL = 512 * 1024 * 1024


def memory_kib():
    values = {}
    with open("/proc/self/smaps_rollup", "r", encoding="ascii") as stream:
        for line in stream:
            fields = line.split()
            if len(fields) >= 2 and fields[0] in ("Rss:", "Pss:"):
                values[fields[0][:-1].lower()] = int(fields[1])
    if "rss" not in values or "pss" not in values:
        raise RuntimeError("smaps_rollup omitted RSS or PSS")
    return values


def child_probe(memory, dirty_pages, page_size, output_fd, release_fd):
    before = resource.getrusage(resource.RUSAGE_SELF)
    started = time.perf_counter_ns()
    for page in range(dirty_pages):
        offset = page * page_size
        memory[offset] = (memory[offset] + 1) & 0xFF
    usage = resource.getrusage(resource.RUSAGE_SELF)
    resident = memory_kib()
    result = {
        "minor_faults": usage.ru_minflt - before.ru_minflt,
        "major_faults": usage.ru_majflt - before.ru_majflt,
        "rss_bytes": resident["rss"] * 1024,
        "pss_bytes": resident["pss"] * 1024,
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


def run(allocation, children, dirty_basis_points):
    if not os.path.exists("/proc/self/smaps_rollup"):
        return {"status": "unsupported", "reason": "/proc/self/smaps_rollup is unavailable"}
    page_size = os.sysconf("SC_PAGE_SIZE")
    rounded = ((allocation + page_size - 1) // page_size) * page_size
    memory = bytearray(rounded)
    for offset in range(0, rounded, page_size):
        memory[offset] = 1

    full_copy_started = time.perf_counter_ns()
    for _ in range(children):
        control = bytearray(memory)
        if len(control) != rounded:
            raise RuntimeError("full-copy control length mismatch")
        del control
    full_copy_ns = time.perf_counter_ns() - full_copy_started

    page_count = rounded // page_size
    dirty_pages = (page_count * dirty_basis_points + 9999) // 10000
    processes = []
    for _ in range(children):
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

    child_results = [read_line(output) for _, output, _ in processes]
    parent = memory_kib()
    for _, _, release in processes:
        os.write(release, b"x")
        os.close(release)
    for pid, _, _ in processes:
        waited, status = os.waitpid(pid, 0)
        if waited != pid or status != 0:
            raise RuntimeError("COW child did not exit cleanly")

    return {
        "status": "succeeded",
        "minor_faults": sum(item["minor_faults"] for item in child_results),
        "major_faults": sum(item["major_faults"] for item in child_results),
        "cow_rss_bytes": parent["rss"] * 1024
        + sum(item["rss_bytes"] for item in child_results),
        "cow_pss_bytes": parent["pss"] * 1024
        + sum(item["pss_bytes"] for item in child_results),
        "full_copy_bytes": rounded * children,
        "full_copy_ns": full_copy_ns,
        "page_size": page_size,
        "dirty_pages_per_child": dirty_pages,
    }


def main():
    if len(sys.argv) != 4:
        raise ValueError("expected allocation_bytes child_fanout dirty_ratio_basis_points")
    allocation, children, dirty = (int(value) for value in sys.argv[1:])
    if allocation < 4096 or allocation > MAX_ALLOCATION:
        raise ValueError("allocation is outside the helper safety bound")
    if children < 1 or children > MAX_CHILDREN:
        raise ValueError("child fan-out is outside the helper safety bound")
    if allocation * children > MAX_TOTAL:
        raise ValueError("allocation times fan-out exceeds the helper safety bound")
    if dirty < 0 or dirty > 10000:
        raise ValueError("dirty ratio is outside 0-10000 basis points")
    print(json.dumps(run(allocation, children, dirty), sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:  # The Rust caller preserves this as a bounded failed sample.
        print(json.dumps({"status": "failed", "error": str(error)}, sort_keys=True))
        sys.exit(1)
