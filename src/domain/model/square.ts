export type Row = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9;
export type Column = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9;

export type Square = {
    row: Row;
    column: Column;
};

export const isSameSquare = (a: Square, b: Square): boolean =>
    a.row === b.row && a.column === b.column;
