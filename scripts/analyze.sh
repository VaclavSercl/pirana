#!/bin/bash
# PIRANA Market Analysis Runner
# Fetches real-time data from Bitfinex and outputs JSON for dashboard

cd /home/wwwenda/workspace/pirana
python3 scripts/market_analysis.py 2>/dev/null
