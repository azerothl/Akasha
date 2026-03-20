"use client";

import { cn } from "@/lib/utils";

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
      </div