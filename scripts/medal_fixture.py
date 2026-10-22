"""Makes tests/fixtures/medal-1234.db (and its -wal): a small database laid out like Medal's
(%APPDATA%\\Medal\\medal-<user>.db), with made-up clips, for the tests of src/sqlite.rs and
src/medal.rs. Made with the real SQLite, in WAL mode, with small pages, so the reader is checked on
interior pages, overflow pages and pages still in the write-ahead log.

    python scripts/medal_fixture.py
"""

import json
import os
import shutil
import sqlite3
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "tests", "fixtures")


def jsonb(v):
    """SQLite's JSONB encoding (what Medal stores): a header byte with the type in the low four
    bits and the payload size in the high four (12-15: the size follows in 1, 2, 4 or 8 bytes)."""

    def head(kind, payload):
        n = len(payload)
        if n <= 11:
            return bytes([(n << 4) | kind]) + payload
        if n <= 0xFF:
            return bytes([0xC0 | kind, n]) + payload
        if n <= 0xFFFF:
            return bytes([0xD0 | kind]) + n.to_bytes(2, "big") + payload
        return bytes([0xE0 | kind]) + n.to_bytes(4, "big") + payload

    if v is None:
        return bytes([0])
    if v is True:
        return bytes([1])
    if v is False:
        return bytes([2])
    if isinstance(v, int):
        return head(3, str(v).encode())
    if isinstance(v, float):
        return head(5, repr(v).encode())
    if isinstance(v, str):
        escaped = json.dumps(v, ensure_ascii=False)[1:-1]
        # TEXTJ (8) when it has escapes, like Medal's "C:\\Medal"; plain TEXT (7) otherwise.
        return head(8 if "\\" in escaped else 7, escaped.encode())
    if isinstance(v, list):
        return head(11, b"".join(jsonb(x) for x in v))
    if isinstance(v, dict):
        return head(12, b"".join(jsonb(k) + jsonb(x) for k, x in v.items()))
    raise TypeError(v)


def clip(game, folder, stamp, title="", event="", fav=False, big=False):
    meta = {
        "title": title,
        "isFavorited": fav,
        "clipDuration": 30.016,
        "contentSize": 1000,
        "width": 1920,
        "height": 1080,
        "codecName": "h264",
        "recorder": {"triggerType": "Keyboard", "eventobj": {"name": event, "tags": {}}},
        "tags": [],
    }
    if big:
        # Long enough to spill onto overflow pages.
        meta["remoteContent"] = {"contentDescription": "x" * 3000, "category": {"categoryName": game}}
    name = f"MedalTV{game.replace(' ', '')}{stamp}.mp4"
    return name, folder, meta


def main():
    os.makedirs(OUT, exist_ok=True)
    work = tempfile.mkdtemp()
    db = os.path.join(work, "medal-1234.db")
    c = sqlite3.connect(db, isolation_level=None)
    c.execute("PRAGMA page_size=1024")
    c.execute("PRAGMA journal_mode=WAL")
    c.execute("PRAGMA wal_autocheckpoint=0")
    c.execute(
        """CREATE TABLE contents (
  created_at INTEGER NOT NULL,
  category_id TEXT,
  video_path TEXT,
  image_path TEXT,
  thumbnail_path TEXT,
  metadata BLOB NOT NULL,
  remote_content_id TEXT UNIQUE,
  local_content_id TEXT UNIQUE NOT NULL,
  parent_id TEXT
)"""
    )
    c.execute("ALTER TABLE contents ADD COLUMN film_reel_path TEXT")
    c.execute("CREATE TABLE key_values (\n  key TEXT UNIQUE NOT NULL,\n  value BLOB NOT NULL\n)")
    c.execute("INSERT INTO key_values VALUES ('LaunchAutomatically', ?)", (jsonb(True),))
    c.execute("INSERT INTO key_values VALUES ('ClipFolder', ?)", (jsonb("Q:\\Medal\\"),))
    c.execute("INSERT INTO key_values VALUES ('Codec', ?)", (jsonb("H264"),))

    root = "Q:\\Medal\\Clips"
    rows = []
    # Many plain rows, so the table needs interior pages.
    for i in range(40):
        rows.append(clip("Testgame", root + "\\Testgame", f"202601011200{i:02d}123"))
    rows.append(clip("Other Game", root + "\\Other Game", "20260102090000000", title="My best shot", fav=True))
    rows.append(clip("Other Game", root + "\\Other Game", "20260102091500000", title="\U0001F44A3K", event="\U0001F44A3K"))
    rows.append(clip("Other Game", root + "\\Other Game", "20260102093000000", big=True))
    c.execute("BEGIN")
    for i, (name, folder, meta) in enumerate(rows):
        c.execute(
            "INSERT INTO contents (created_at, category_id, video_path, thumbnail_path, metadata, local_content_id) VALUES (?,?,?,?,?,?)",
            (
                1767268800 + i * 60,
                "cat-" + meta.get("title", "")[:3],
                folder + "\\" + name,
                "Q:\\Medal\\Thumbnails\\" + name[:-4] + ".jpg",
                jsonb(meta),
                f"00000000-0000-0000-0000-{i:012d}",
            ),
        )
    # A screenshot and a cloud-only post (no file on this computer).
    c.execute(
        "INSERT INTO contents (created_at, image_path, metadata, local_content_id) VALUES (?,?,?,?)",
        (1767270000, "Q:\\Medal\\Screenshots\\Testgame\\MedalTVTestgame20260101123000000.png", jsonb({"title": ""}), "shot-1"),
    )
    c.execute(
        "INSERT INTO contents (created_at, thumbnail_path, metadata, remote_content_id, local_content_id) VALUES (?,?,?,?,?)",
        (1767270100, "Q:\\Medal\\Thumbnails\\cloud.jpg", jsonb({"title": "cloud"}), "remote-1", "cloud-1"),
    )
    c.execute("COMMIT")
    # Everything so far into the file; what follows stays in the -wal only.
    c.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    name, folder, meta = clip("Late Game", root + "\\Late Game", "20260103100000000", title="In the log only")
    c.execute(
        "INSERT INTO contents (created_at, video_path, metadata, local_content_id) VALUES (?,?,?,?)",
        (1767434400, folder + "\\" + name, jsonb(meta), "wal-1"),
    )
    c.execute("UPDATE key_values SET value = ? WHERE key = 'Codec'", (jsonb("H265"),))
    # Copied while still open: closing would fold the log into the file and delete it.
    for ext in ("", "-wal"):
        shutil.copyfile(db + ext, os.path.join(OUT, "medal-1234.db" + ext))
    c.close()
    shutil.rmtree(work, ignore_errors=True)
    for ext in ("", "-wal"):
        p = os.path.join(OUT, "medal-1234.db" + ext)
        print(p, os.path.getsize(p))


if __name__ == "__main__":
    main()
