"use client";

import { useState } from "react";
import { cn } from "@/lib/utils";

export default function AddTransaction({ onAdd }: { onAdd: (data: any) => void }) {
  const [description, setDescription] = useState("");
  const [amount, setAmount] = useState("");
  const [type, setType] = useState<"income" | "expense">("expense");

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!description || !amount) return;

    onAdd({
      description,
      amount: parseFloat(amount),
      type,
    });

    setDescription("");
    setAmount("");
  };

  return (
    <div className="bg-white p-6 rounded-xl shadow-sm border border-gray-200">
      <h2 className="text-xl font-semibold mb-4">Ajouter une transaction</h2>
      <form onSubmit={handleSubmit} className="space-y-4">
        <div>
          <label htmlFor="description" className="block text-sm font-medium text-gray-700 mb-1">
            Description
          </label>
          <input
            id="description"
            type="text"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="Ex: Salaire, Courses..."
            className="w-full px-4 py-2 border border-gray-300 rounded-lg focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none transition"
          />
        </div>

        <div>
          <label htmlFor="amount" className="block text-sm font-medium text-gray-700 mb-1">
            Montant (€)
          </label>
          <input
            id="amount"
            type="number"
            step="0.01"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            placeholder="0.00"
            className="w-full px-4 py-2 border border-gray-300 rounded-lg focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none transition"
          />
        </div>

        <div>
          <label className="block text-sm font-medium text-gray-700 mb-2">Type</label>
          <div className="flex gap-4">
            <label className="flex items-center space-x-2 cursor-pointer">
              <input
                type="radio"
                name="type"
                value="expense"
                checked={type === "expense"}
                onChange={() => setType("expense")}
                className="w-4 h-4 text-red-600 focus:ring-red-500"
              />
              <span className="text-gray-700">Dépense</span>
            </label>
            <label className="flex items-center space-x-2 cursor-pointer">
              <input
                type="radio"
                name="type"
                value="income"
                checked={type === "income"}
                onChange={() => setType("income")}
                className="w-4 h-4 text-green-600 focus:ring-green-500"
              />
              <span className="text-gray-700">Revenu</span>
            </label>
          </div>
        </div>

        <button
          type="submit"
          className="w-full bg-blue-600 text-white py-2 px-4 rounded-lg hover:bg-blue-700 transition font-medium"
        >
          Ajouter
        </button>
      </form>
    </div>
  );
}