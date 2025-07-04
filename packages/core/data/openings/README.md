# Opening Book Data

This directory contains processed opening book data for the Shogi AI engine.

## Data Source

The opening book data was derived from YaneuraOu's `user_book1.db` file:
- Original source: https://github.com/yaneurao/YaneuraOu
- License: MIT License
- Processing script: `packages/core/scripts/convertOpeningBook.ts`

## File Format

- `openings-XXX.gzip`: Compressed JSON files containing opening positions and moves
- `index.json`: Index file listing all opening files
- `standard-openings.gzip`: Standard openings for the first few moves (if present)

## Usage

These files are automatically loaded by the AI engine during initialization.
The data is split into multiple files for efficient loading and memory management.

## Regenerating the Data

If you have access to a `user_book1.db` file, you can regenerate this data:

```bash
cd packages/core
npm run tsx scripts/convertOpeningBook.ts ../../../user_book1.db
```

Note: The original `user_book1.db` file is not included in this repository.