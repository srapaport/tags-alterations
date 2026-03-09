#!/usr/bin/env python3
"""
Query nixpkgs git history to find when packages were created/updated.
This helps determine if a nixpkg was added before or after a tag alteration.
"""

import subprocess
import json
import pandas as pd
from pathlib import Path
from datetime import datetime
import re
from typing import Optional, Dict, List
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from tqdm import tqdm


class NixpkgsGitQuery:
    """Query nixpkgs repository for package history"""
    
    def __init__(self, nixpkgs_path: str):
        """
        Args:
            nixpkgs_path: Path to a cloned nixpkgs repository
        """
        self.nixpkgs_path = Path(nixpkgs_path)
        if not (self.nixpkgs_path / ".git").exists():
            raise ValueError(f"{nixpkgs_path} is not a git repository")
    
    def find_package_paths(self, attr_path: str) -> List[str]:
        """
        Find possible file paths for a package attribute.
        nixpkgs uses several conventions:
        - pkgs/by-name/ar/armips/package.nix (new layout)
        - pkgs/applications/...
        - pkgs/development/...
        - pkgs/tools/...
        etc.
        """
        # Try to find files matching the attr_path
        patterns = [
            f"**/by-name/**/{attr_path}/**",
            f"**/{attr_path}/**",
            f"**/{attr_path}.nix",
        ]
        
        paths = []
        for pattern in patterns:
            try:
                result = subprocess.run(
                    ["git", "ls-files", pattern],
                    cwd=self.nixpkgs_path,
                    capture_output=True,
                    text=True,
                    check=True
                )
                found = [p for p in result.stdout.strip().split('\n') if p]
                paths.extend(found)
            except subprocess.CalledProcessError:
                continue
        
        return sorted(set(paths))
    
    def get_first_commit_date(self, file_path: str) -> Optional[datetime]:
        """
        Get the date of the first commit that introduced this file.
        
        Args:
            file_path: Relative path to file in nixpkgs repo
            
        Returns:
            datetime of first commit, or None if not found
        """
        try:
            # Use --diff-filter=A to find only the commit that *added* the file.
            # Do NOT use --follow here: it scans the entire history for renames and
            # causes timeouts on large repos like nixpkgs.
            result = subprocess.run(
                ["git", "log", "--format=%aI", "--diff-filter=A", "--", file_path],
                cwd=self.nixpkgs_path,
                capture_output=True,
                text=True, 
                check=True,
                timeout=30
            )
            
            date_str = result.stdout.strip().split('\n')[-1]  # oldest if multiple
            if date_str:
                return datetime.fromisoformat(date_str.replace('Z', '+00:00'))
            return None
            
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, ValueError) as e:
            print(f"Error querying {file_path}: {e}")
            return None
    
    def get_last_commit_date(self, file_path: str) -> Optional[datetime]:
        """
        Get the date of the most recent commit that modified this file.
        
        Args:
            file_path: Relative path to file in nixpkgs repo
            
        Returns:
            datetime of last commit, or None if not found
        """
        try:
            result = subprocess.run(
                ["git", "log", "-1", "--follow", "--format=%aI", "--", file_path],
                cwd=self.nixpkgs_path,
                capture_output=True,
                text=True,
                check=True,
                timeout=10
            )
            
            date_str = result.stdout.strip()
            if date_str:
                return datetime.fromisoformat(date_str.replace('Z', '+00:00'))
            return None
            
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, ValueError) as e:
            print(f"Error querying {file_path}: {e}")
            return None
    
    def get_commit_before_date(self, file_path: str, target_date: datetime) -> Optional[Dict]:
        """
        Find if the file existed and what version was present before a target date.
        
        Args:
            file_path: Relative path to file in nixpkgs repo
            target_date: The cutoff date
            
        Returns:
            Dict with commit info, or None if file didn't exist before that date
        """
        try:
            # Get the last commit before target_date
            date_str = target_date.strftime("%Y-%m-%d")
            result = subprocess.run(
                ["git", "log", "-1", "--before", date_str, "--format=%aI|%H|%s", "--", file_path],
                cwd=self.nixpkgs_path,
                capture_output=True,
                text=True,
                check=True,
                timeout=10
            )
            
            line = result.stdout.strip()
            if line:
                parts = line.split('|', 2)
                return {
                    'date': datetime.fromisoformat(parts[0].replace('Z', '+00:00')),
                    'commit': parts[1],
                    'message': parts[2] if len(parts) > 2 else ''
                }
            return None
            
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, ValueError) as e:
            print(f"Error querying {file_path} before {target_date}: {e}")
            return None
    
    def analyze_package(self, attr_path: str) -> Dict:
        """
        Analyze a package's history.
        
        Returns:
            Dict with package history info
        """
        paths = self.find_package_paths(attr_path)
        
        result = {
            'attr_path': attr_path,
            'found_paths': paths,
            'first_commit_date': None,
            'last_commit_date': None,
            'primary_path': None
        }
        
        if not paths:
            return result
        
        # Use the first path found (usually the most relevant)
        primary_path = paths[0]
        result['primary_path'] = primary_path
        
        result['first_commit_date'] = self.get_first_commit_date(primary_path)
        result['last_commit_date'] = self.get_last_commit_date(primary_path)
        
        return result


def process_correlation_data(correlation_csv: str, nixpkgs_path: str, output_csv: str, workers: int = 8):
    """
    Process correlation data and enrich with nixpkgs dates.
    
    Args:
        correlation_csv: Path to step2_altered_tag_matches.csv
        nixpkgs_path: Path to nixpkgs git repository
        output_csv: Path to save enriched results
        workers: Number of parallel threads for git queries
    """
    # Load correlation data
    df = pd.read_csv(correlation_csv)
    print(f"Loaded {len(df)} correlation matches")
    
    # Get unique attr_paths
    unique_attrs = df['attr_path'].unique()
    print(f"Found {len(unique_attrs)} unique nixpkgs attributes")
    
    # Initialize git query
    query = NixpkgsGitQuery(nixpkgs_path)
    
    # Query git history for each package in parallel
    results = [None] * len(unique_attrs)
    with ThreadPoolExecutor(max_workers=workers) as executor:
        futures = {executor.submit(query.analyze_package, attr): i for i, attr in enumerate(unique_attrs)}
        with tqdm(total=len(unique_attrs), desc="Querying nixpkgs history") as pbar:
            for future in as_completed(futures):
                results[futures[future]] = future.result()
                pbar.update(1)
    
    # Create dataframe with results
    dates_df = pd.DataFrame(results)
    
    # Merge with original correlation data
    enriched = df.merge(
        dates_df[['attr_path', 'first_commit_date', 'last_commit_date', 'primary_path']],
        on='attr_path',
        how='left'
    )
    
    # Convert timestamp columns to datetime for comparison (UTC-aware to match git dates)
    enriched['old_snap_timestamp'] = pd.to_datetime(enriched['old_snap_timestamp'], unit='s', utc=True)
    enriched['alteration_detected_at'] = pd.to_datetime(enriched['alteration_detected_at'], unit='s', utc=True)
    # Convert git dates to UTC-aware pandas timestamps (None → NaT) for vectorized ops
    enriched['first_commit_date'] = pd.to_datetime(enriched['first_commit_date'], utc=True)
    enriched['last_commit_date'] = pd.to_datetime(enriched['last_commit_date'], utc=True)

    # Add comparison columns
    enriched['nixpkg_before_alteration'] = enriched['first_commit_date'] < enriched['alteration_detected_at']
    enriched['nixpkg_before_old_snap'] = enriched['first_commit_date'] < enriched['old_snap_timestamp']
    
    # Calculate time differences (in days)
    enriched['days_from_first_commit_to_alteration'] = (
        (enriched['alteration_detected_at'] - enriched['first_commit_date']).dt.total_seconds() / 86400
    )
    
    # Save enriched data
    enriched.to_csv(output_csv, index=False)
    print(f"\nEnriched data saved to {output_csv}")
    
    # Print summary statistics
    print("\n" + "="*60)
    print("SUMMARY")
    print("="*60)
    print(f"Total matches: {len(enriched)}")
    print(f"Packages found in nixpkgs: {enriched['first_commit_date'].notna().sum()}")
    print(f"Packages NOT found: {enriched['first_commit_date'].isna().sum()}")
    
    if enriched['first_commit_date'].notna().any():
        print(f"\nNixpkg added BEFORE alteration detected: {enriched['nixpkg_before_alteration'].sum()}")
        print(f"Nixpkg added AFTER alteration detected: {(~enriched['nixpkg_before_alteration']).sum()}")
        print(f"\nNixpkg added BEFORE old snapshot: {enriched['nixpkg_before_old_snap'].sum()}")
        print(f"Nixpkg added AFTER old snapshot: {(~enriched['nixpkg_before_old_snap']).sum()}")
        
        print(f"\nMedian days from first commit to alteration: "
              f"{enriched['days_from_first_commit_to_alteration'].median():.1f}")
    
    return enriched


def main():
    parser = argparse.ArgumentParser(
        description="Query nixpkgs git history for package creation/update dates"
    )
    parser.add_argument(
        "--correlation-csv",
        default="step2_altered_tag_matches.csv",
        help="Path to correlation CSV file"
    )
    parser.add_argument(
        "--nixpkgs-path",
        required=True,
        help="Path to cloned nixpkgs repository"
    )
    parser.add_argument(
        "--output",
        default="step3_with_nixpkg_dates.csv",
        help="Output CSV file path"
    )
    parser.add_argument(
        "--workers",
        type=int,
        default=8,
        help="Number of parallel threads for git queries (default: 8)"
    )
    
    args = parser.parse_args()
    
    process_correlation_data(
        args.correlation_csv,
        args.nixpkgs_path,
        args.output,
        workers=args.workers,
    )


if __name__ == "__main__":
    main()
