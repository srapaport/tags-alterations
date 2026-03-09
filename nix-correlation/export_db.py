# export_db.py
import sqlite3
import pandas as pd
from pathlib import Path
from datetime import datetime

DB_PATH  = "../data/tags_alterations_full_2025-10_v2.db"   # adjust if needed
OUT_PATH = Path("tags_df_bis.pkl")
OUT_PATH.parent.mkdir(exist_ok=True)

# Filter: old_snap_timestamp must be >= this date
MIN_DATE = "2015-09-17"
MIN_TIMESTAMP = int(datetime.fromisoformat(MIN_DATE).timestamp())

print(f"[*] Connecting to {DB_PATH}")
con = sqlite3.connect(DB_PATH)

tags_df = pd.read_sql_query(f"""
    SELECT DISTINCT
        origin_url,
        tag_name   AS tag_bare,
        old_snapshot,
        old_snap_timestamp,
        new_snapshot,
        new_snap_timestamp
    FROM tag_inconsistencies
    WHERE old_snap_timestamp >= {MIN_TIMESTAMP}
""", con)

con.close()

print(f"[*] Rows fetched : {len(tags_df):,}")
print(f"[*] Saving to    : {OUT_PATH}")

tags_df.to_pickle(OUT_PATH)

print("[+] Done")
print(tags_df.head(5).to_string())
