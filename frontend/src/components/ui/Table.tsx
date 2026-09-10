import type { ReactNode } from "react";

export interface TableColumn<Row> {
  header: string;
  key: string;
  render: (row: Row) => ReactNode;
}

interface TableProps<Row> {
  caption: string;
  columns: TableColumn<Row>[];
  getRowKey: (row: Row) => string;
  rows: Row[];
}

export function Table<Row>({
  caption,
  columns,
  getRowKey,
  rows,
}: TableProps<Row>) {
  return (
    <div className="table-wrap">
      <table className="data-table">
        <caption>{caption}</caption>
        <thead>
          <tr>
            {columns.map((column) => (
              <th key={column.key} scope="col">
                {column.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={getRowKey(row)}>
              {columns.map((column) => (
                <td key={column.key}>{column.render(row)}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
