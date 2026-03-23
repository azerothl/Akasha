"use client";

export default function TransactionTable({
  transactions,
  onDelete,
}: {
  transactions: any[];
  onDelete: (id: string) => void;
}) {
  if (transactions.length === 0) {
    return (
      <div className="bg-white p-8 rounded-xl shadow-sm border border-gray-200 text-center text-gray-500">
        Aucune transaction pour le moment.
      </div>
    );
  }

  return (
    <div className="bg-white p-4 rounded-xl shadow-sm border border-gray-200">
      <div className="overflow-x-auto">
        <table className="min-w-full text-sm">
          <thead>
            <tr className="text-left text-gray-500 border-b border-gray-200">
              <th className="py-2 pr-4">ID</th>
              <th className="py-2 pr-4">Détails</th>
              <th className="py-2 text-right">Actions</th>
            </tr>
          </thead>
          <tbody>
            {transactions.map((transaction: any) => (
              <tr key={transaction.id} className="border-b border-gray-100 last:border-b-0">
                <td className="py-2 pr-4 align-middle text-gray-900">
                  {String(transaction.id)}
                </td>
                <td className="py-2 pr-4 align-middle text-gray-700">
                  {typeof transaction === "object"
                    ? JSON.stringify(transaction)
                    : String(transaction)}
                </td>
                <td className="py-2 align-middle text-right">
                  <button
                    type="button"
                    className="inline-flex items-center rounded-md border border-red-200 px-3 py-1 text-xs font-medium text-red-700 hover:bg-red-50"
                    onClick={() => onDelete(String(transaction.id))}
                  >
                    Supprimer
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}