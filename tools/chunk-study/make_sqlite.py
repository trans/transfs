#!/usr/bin/env python3
"""Generate SQLite snapshots for the chunk study (whitepaper §6, gate 2).

Builds an application-style database of about 100 MB from a fixed seed, then
saves a snapshot after each kind of change with SQLite's online backup API,
which keeps the source's page layout:

  base.sqlite     the starting database
  updates.sqlite  ordinary use: some rows edited, events appended and deleted
  bulk.sqlite     a bulk insert, about 10% more rows
  vacuum.sqlite   bulk.sqlite after VACUUM, on a disposable copy

Usage: make_sqlite.py OUTPUT_DIR
"""
import json
import os
import random
import shutil
import sqlite3
import sys

SEED = 20261005
ITEMS = 120_000
EVENTS = 260_000

rng = random.Random(SEED)
WORDS = [
    "".join(rng.choice("abcdefghijklmnopqrstuvwxyz") for _ in range(rng.randint(2, 10)))
    for _ in range(5000)
]
KINDS = ["note", "task", "contact", "event", "message", "document"]
TAGS = [f"tag{i}" for i in range(200)]


def text(words):
    return " ".join(rng.choice(WORDS) for _ in range(words))


def add_items(db, count, start_ts):
    for i in range(count):
        ts = start_ts + i
        cur = db.execute(
            "INSERT INTO items (kind, title, body, created, updated) VALUES (?, ?, ?, ?, ?)",
            (rng.choice(KINDS), text(rng.randint(2, 8)), text(rng.randint(20, 120)), ts, ts),
        )
        for tag in rng.sample(TAGS, rng.randint(0, 4)):
            db.execute("INSERT INTO tags (item_id, tag) VALUES (?, ?)", (cur.lastrowid, tag))


def add_events(db, count, start_ts):
    max_item = db.execute("SELECT max(id) FROM items").fetchone()[0]
    for i in range(count):
        payload = {"action": rng.choice(["view", "edit", "share", "move"]), "note": text(rng.randint(3, 25))}
        db.execute(
            "INSERT INTO events (item_id, ts, payload) VALUES (?, ?, ?)",
            (rng.randint(1, max_item), start_ts + i, json.dumps(payload)),
        )


def snapshot(db, path):
    if os.path.exists(path):
        os.remove(path)
    out = sqlite3.connect(path)
    db.backup(out)
    out.close()
    print(f"{os.path.basename(path)}: {os.path.getsize(path) / 2**20:.1f} MiB")


def main():
    out_dir = sys.argv[1]
    os.makedirs(out_dir, exist_ok=True)
    work = os.path.join(out_dir, "work.sqlite")
    if os.path.exists(work):
        os.remove(work)
    db = sqlite3.connect(work)
    db.executescript(
        """
        CREATE TABLE items (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, title TEXT,
                            body TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL);
        CREATE TABLE tags (item_id INTEGER NOT NULL, tag TEXT NOT NULL);
        CREATE TABLE events (id INTEGER PRIMARY KEY, item_id INTEGER NOT NULL,
                             ts INTEGER NOT NULL, payload TEXT);
        CREATE INDEX items_kind_updated ON items (kind, updated);
        CREATE INDEX tags_tag ON tags (tag, item_id);
        CREATE INDEX events_item_ts ON events (item_id, ts);
        """
    )

    with db:
        add_items(db, ITEMS, 1_700_000_000)
        add_events(db, EVENTS, 1_700_000_000)
    snapshot(db, os.path.join(out_dir, "base.sqlite"))

    # Ordinary use: edit 500 items, append 2,000 events, delete 300 old ones.
    with db:
        for item in rng.sample(range(1, ITEMS + 1), 500):
            db.execute("UPDATE items SET body = ?, updated = ? WHERE id = ?",
                       (text(rng.randint(20, 120)), 1_800_000_000, item))
        add_events(db, 2_000, 1_800_000_000)
        for event in rng.sample(range(1, EVENTS + 1), 300):
            db.execute("DELETE FROM events WHERE id = ?", (event,))
    snapshot(db, os.path.join(out_dir, "updates.sqlite"))

    # Bulk insert: about 10% more rows.
    with db:
        add_items(db, ITEMS // 10, 1_900_000_000)
        add_events(db, EVENTS // 10, 1_900_000_000)
    snapshot(db, os.path.join(out_dir, "bulk.sqlite"))
    db.close()

    # VACUUM repacks every page; run it only on a disposable copy.
    vacuum = os.path.join(out_dir, "vacuum.sqlite")
    shutil.copyfile(os.path.join(out_dir, "bulk.sqlite"), vacuum)
    v = sqlite3.connect(vacuum)
    v.execute("VACUUM")
    v.close()
    print(f"vacuum.sqlite: {os.path.getsize(vacuum) / 2**20:.1f} MiB")
    os.remove(work)


if __name__ == "__main__":
    main()
