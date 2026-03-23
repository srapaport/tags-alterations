import sqlite3
import pandas as pd
import numpy as np
import pickle
import re
import matplotlib.pyplot as plt
import matplotlib.dates as mdates
from datetime import datetime
pd.set_option('display.max_colwidth', None)
pd.set_option('display.max_columns', None)

print("Libraries imported successfully")

# Parse tags_count.log file
df_all = pd.read_pickle("../data/tags_alteration_full_2025-10_dm.pkl")
df = df_all[df_all['status'] != 'non-legit'].copy()

tags_monthly_data = []

with open('../data/counts/tags_count.log', 'r') as f:
    # Skip lines until we find the header
    for line in f:
        if line.strip().startswith('Month'):
            # Skip the separator line
            next(f)
            break
    
    # Parse data lines
    for line in f:
        line = line.strip()
        if not line:
            continue
        
        # Parse lines with format: "1970-01    1621    307883    0    309504"
        parts = line.split()
        if len(parts) >= 5:
            try:
                month = parts[0]
                lightweight = int(parts[1])
                annotated = int(parts[2])
                unknown = int(parts[3])
                total = int(parts[4])
                
                tags_monthly_data.append({
                    'month': month,
                    'lightweight': lightweight,
                    'annotated': annotated,
                    'unknown': unknown,
                    'total': total
                })
            except ValueError:
                # Skip lines that don't match expected format
                continue

# Create DataFrame
tags_monthly_df = pd.DataFrame(tags_monthly_data)

print(f"Loaded {len(tags_monthly_df):_} months of data")
print(f"\nDate range: {tags_monthly_df['month'].iloc[0]} to {tags_monthly_df['month'].iloc[-1]}")
print(f"\nFirst few rows:")
print(tags_monthly_df.head())
print(f"\nLast few rows:")
print(tags_monthly_df.tail())

# Check temporal columns in alterations dataframe
print("Alterations DataFrame columns:")
print(df.columns.tolist())
print(f"\nSample rows:")
df.head(3)

# Convert timestamps to datetime and extract year-month
df['alteration_date'] = pd.to_datetime(df['new_snap_timestamp'], unit='s', errors='coerce')
df['alteration_month'] = df['alteration_date'].dt.strftime('%Y-%m')

# Group alterations by month
alterations_by_month = df.groupby('alteration_month').agg({
    'tag_name': 'count',  # Total alterations
    'type': lambda x: (x == 'annotated').sum()  # Annotated alterations
}).rename(columns={'tag_name': 'total_alterations', 'type': 'annotated_alterations'})

# Calculate lightweight alterations
alterations_by_month['lightweight_alterations'] = (
    alterations_by_month['total_alterations'] - alterations_by_month['annotated_alterations']
)

# Reset index to make month a column
alterations_by_month = alterations_by_month.reset_index()

# Calculate CUMULATIVE tags (tags existing up to that month)
tags_monthly_df['cumulative_total'] = tags_monthly_df['total'].cumsum()
tags_monthly_df['cumulative_lightweight'] = tags_monthly_df['lightweight'].cumsum()
tags_monthly_df['cumulative_annotated'] = tags_monthly_df['annotated'].cumsum()

# Merge with monthly tags data
temporal_analysis = tags_monthly_df.merge(
    alterations_by_month, 
    left_on='month', 
    right_on='alteration_month', 
    how='left'
)

# Fill NaN values with 0 for months with no alterations
temporal_analysis['total_alterations'] = temporal_analysis['total_alterations'].fillna(0).astype(int)
temporal_analysis['annotated_alterations'] = temporal_analysis['annotated_alterations'].fillna(0).astype(int)
temporal_analysis['lightweight_alterations'] = temporal_analysis['lightweight_alterations'].fillna(0).astype(int)

# Calculate normalized metrics using CUMULATIVE tags (alterations per 1000 existing tags)
temporal_analysis['alterations_per_1000'] = (
    temporal_analysis['total_alterations'] / temporal_analysis['cumulative_total'] * 1000
)
temporal_analysis['annotated_alt_per_1000'] = (
    temporal_analysis['annotated_alterations'] / temporal_analysis['cumulative_annotated'] * 1000
).replace([np.inf, -np.inf], 0)
temporal_analysis['lightweight_alt_per_1000'] = (
    temporal_analysis['lightweight_alterations'] / temporal_analysis['cumulative_lightweight'] * 1000
).replace([np.inf, -np.inf], 0)

# Drop the duplicate month column
temporal_analysis = temporal_analysis.drop(columns=['alteration_month'], errors='ignore')

print("=" * 70)
print("TEMPORAL ANALYSIS (EXCLUDING NON-LEGIT)")
print("=" * 70)
print(f"Total months analyzed: {len(temporal_analysis):_}")
print(f"Months with alterations: {(temporal_analysis['total_alterations'] > 0).sum():_}")
print(f"\nSummary statistics (normalized per 1000 EXISTING tags):")
print(f"  Mean alterations per 1000 tags: {temporal_analysis['alterations_per_1000'].mean():.4f}")
print(f"  Median alterations per 1000 tags: {temporal_analysis['alterations_per_1000'].median():.4f}")
print(f"  Max alterations per 1000 tags: {temporal_analysis['alterations_per_1000'].max():.4f}")
print(f"\nFinal cumulative totals:")
print(f"  Total tags existing: {temporal_analysis['cumulative_total'].iloc[-1]:_}")
print(f"  Lightweight: {temporal_analysis['cumulative_lightweight'].iloc[-1]:_}")
print(f"  Annotated: {temporal_analysis['cumulative_annotated'].iloc[-1]:_}")
print("\n" + "=" * 70)

temporal_analysis.head(10)

# Show months with highest alteration rates
print("=" * 70)
print("TOP 20 MONTHS BY ALTERATION RATE (per 1000 EXISTING tags)")
print("=" * 70)
top_months = temporal_analysis.nlargest(20, 'alterations_per_1000')[
    ['month', 'cumulative_total', 'total_alterations', 'alterations_per_1000',
     'annotated_alt_per_1000', 'lightweight_alt_per_1000']
]
print(top_months.to_string(index=False))

print("\n" + "=" * 70)
print("RECENT TRENDS (Last 24 months with data)")
print("=" * 70)
recent_data = temporal_analysis[temporal_analysis['total'] > 0].tail(24)
print(f"Recent months analyzed: {len(recent_data)}")
print(f"Months with alterations: {(recent_data['total_alterations'] > 0).sum()}")
print(f"Mean alterations per 1000 existing tags: {recent_data['alterations_per_1000'].mean():.4f}")
print(f"Total alterations: {recent_data['total_alterations'].sum():_}")
print(f"Total tags created in period: {recent_data['total'].sum():_}")
print(f"Cumulative tags at end of period: {recent_data['cumulative_total'].iloc[-1]:_}")

import matplotlib.pyplot as plt
import matplotlib.dates as mdates
from datetime import datetime

# Convert month strings to datetime for better plotting
temporal_analysis['month_date'] = pd.to_datetime(temporal_analysis['month'] + '-01')

# Filter to only months with tag data AND between 2015-2026
plot_data = temporal_analysis[
    (temporal_analysis['total'] > 0) & 
    (temporal_analysis['month_date'] >= '2015-01-01') &
    (temporal_analysis['month_date'] < '2026-01-01')
].copy()

print(f"Plotting data from {plot_data['month_date'].min().strftime('%Y-%m')} to {plot_data['month_date'].max().strftime('%Y-%m')}")
print(f"Total months in range: {len(plot_data)}")
print()

# Create figure with 2 subplots
fig, (ax1, ax2) = plt.subplots(2, 1, figsize=(14, 10))

# ============================================================
# FIGURE 1: Cumulative Evolution
# ============================================================
ax1_twin = ax1.twinx()

# Plot cumulative tags on left y-axis
line1 = ax1.plot(plot_data['month_date'], plot_data['cumulative_total'], 
                 'b-', linewidth=2, label='Cumulative Tags', alpha=0.7)
ax1.fill_between(plot_data['month_date'], 0, plot_data['cumulative_total'], 
                  alpha=0.1, color='blue')

# Plot cumulative alterations on right y-axis
cumulative_alterations = plot_data['total_alterations'].cumsum()
line2 = ax1_twin.plot(plot_data['month_date'], cumulative_alterations, 
                      'r-', linewidth=2, label='Cumulative Alterations', alpha=0.7)
ax1_twin.fill_between(plot_data['month_date'], 0, cumulative_alterations, 
                       alpha=0.1, color='red')

# Formatting
ax1.set_xlabel('Date', fontsize=12, fontweight='bold')
ax1.set_ylabel('Cumulative Tags', fontsize=12, fontweight='bold', color='blue')
ax1_twin.set_ylabel('Cumulative Alterations', fontsize=12, fontweight='bold', color='red')
ax1.tick_params(axis='y', labelcolor='blue')
ax1_twin.tick_params(axis='y', labelcolor='red')
ax1.set_title('Cumulative Evolution: Tags vs Alterations (2015-2025)', fontsize=14, fontweight='bold', pad=20)
ax1.grid(True, alpha=0.3)
ax1.xaxis.set_major_formatter(mdates.DateFormatter('%Y'))
ax1.xaxis.set_major_locator(mdates.YearLocator(1))

# Add combined legend
lines = line1 + line2
labels = [l.get_label() for l in lines]
ax1.legend(lines, labels, loc='upper left', fontsize=10)

# ============================================================
# FIGURE 2: Monthly (Non-Cumulative) Evolution
# ============================================================
ax2_twin = ax2.twinx()

# Plot monthly tags created on left y-axis
line3 = ax2.plot(plot_data['month_date'], plot_data['total'], 
                 'b-', linewidth=1.5, label='Monthly Tags Created', alpha=0.7)
ax2.fill_between(plot_data['month_date'], 0, plot_data['total'], 
                  alpha=0.1, color='blue')

# Plot monthly alterations on right y-axis
line4 = ax2_twin.plot(plot_data['month_date'], plot_data['total_alterations'], 
                      'r-', linewidth=1.5, label='Monthly Alterations', alpha=0.7)
ax2_twin.fill_between(plot_data['month_date'], 0, plot_data['total_alterations'], 
                       alpha=0.1, color='red')

# Formatting
ax2.set_xlabel('Date', fontsize=12, fontweight='bold')
ax2.set_ylabel('Monthly Tags Created', fontsize=12, fontweight='bold', color='blue')
ax2_twin.set_ylabel('Monthly Alterations', fontsize=12, fontweight='bold', color='red')
ax2.tick_params(axis='y', labelcolor='blue')
ax2_twin.tick_params(axis='y', labelcolor='red')
ax2.set_title('Monthly Evolution: Tags Created vs Alterations (2015-2025)', fontsize=14, fontweight='bold', pad=20)
ax2.grid(True, alpha=0.3)
ax2.xaxis.set_major_formatter(mdates.DateFormatter('%Y'))
ax2.xaxis.set_major_locator(mdates.YearLocator(1))

# Add combined legend
lines = line3 + line4
labels = [l.get_label() for l in lines]
ax2.legend(lines, labels, loc='upper left', fontsize=10)

plt.tight_layout()
plt.savefig("old_te.png")

print(f"\nCumulative alterations in period: {cumulative_alterations.iloc[-1]:_}")
print(f"Cumulative tags at end of period: {plot_data['cumulative_total'].iloc[-1]:_}")
print(f"Alteration rate in period: {(cumulative_alterations.iloc[-1] / plot_data['cumulative_total'].iloc[-1] * 100):.4f}%")

