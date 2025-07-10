#!/bin/bash

# Generate and copy opening book files from rust-core to web

set -e

echo "🔄 Copying opening book files from converted_openings..."

# Check if converted_openings directory exists
if [ ! -d "converted_openings" ]; then
    echo "⚠️  Warning: converted_openings directory not found!"
    echo "Skipping opening book copy (likely in CI/CD environment)"
    exit 0
fi

# Check for required files
REQUIRED_FILES=(
    "opening_book_early.binz"
    "opening_book_web.binz"
    "opening_book_full.binz"
)

echo "📋 Checking for required files..."
for file in "${REQUIRED_FILES[@]}"; do
    if [ ! -f "converted_openings/$file" ]; then
        echo "⚠️  Warning: $file not found in converted_openings/"
    fi
done

echo "📋 Copying files to web package..."
mkdir -p ../web/public/data

# Copy available .binz files
if ls converted_openings/*.binz 1> /dev/null 2>&1; then
    cp converted_openings/*.binz ../web/public/data/
    
    # Rename files to match expected names
    if [ -f "../web/public/data/opening_book_web.binz" ] && [ ! -f "../web/public/data/opening_book_standard.binz" ]; then
        echo "📝 Creating opening_book_standard.binz from opening_book_web.binz..."
        cp ../web/public/data/opening_book_web.binz ../web/public/data/opening_book_standard.binz
    fi
    
    echo "✅ Opening book files copied successfully!"
    echo "📁 Files in web/public/data:"
    ls -lh ../web/public/data/*.binz 2>/dev/null || echo "No .binz files found"
else
    echo "⚠️  No .binz files found in converted_openings/"
    echo "Skipping opening book copy (likely in CI/CD environment)"
    exit 0
fi
