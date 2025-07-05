#!/bin/bash

# Generate and copy opening book files from rust-core to web

set -e

echo "🔄 Generating opening book files..."

# Create data directory if it doesn't exist
mkdir -p data/openings

# Generate opening book files (placeholder for now)
# TODO: Run actual conversion commands here when ready
# cargo run --bin convert_opening_book -- [options]

echo "📦 Creating placeholder opening book files..."
touch data/openings/opening_book_early.bin.gz
touch data/openings/opening_book_standard.bin.gz
touch data/openings/opening_book_full.bin.gz

echo "📋 Copying files to web package..."
mkdir -p ../web/public/data
cp data/openings/*.bin.gz ../web/public/data/

echo "✅ Opening book files copied successfully!"