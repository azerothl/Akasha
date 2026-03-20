"use client";

import { useState, useEffect } from "react";
import TransactionTable from "@/components/TransactionTable";
import AddTransaction from "@/components/AddTransaction";

interface Transaction {
  id: string;
  description: string;
  amount: number;
  type: "income" | "expense";
  date: string;
}

export default function Home() {
  const [transactions, setTransactions] = useState<Transaction[]>([]);

  // Charger les transactions au démarrage
  useEffect(() => {
    const saved = localStorage.getItem("transactions");
    if (saved) {
      setTransactions(JSON.parse(saved));
    }
  }, []);

  // Sauvegarder les transactions
  const saveTransactions = (data: Transaction[]) => {
    setTransactions(data);
    localStorage.setItem("transactions", JSON.stringify(data));
  };

  const addTransaction = (transaction: Omit<Transaction, "id">) => {
    const newTransaction: Transaction = {
      ...transaction,
      id: Date.now().toString(),
      date: new Date().toISOString().split("T")[0],
    };
    saveTransactions([...transactions, newTransaction]);
  };

  const deleteTransaction = (id: string) => {
    const updated = transactions.filter((t) => t.id !== id);
    saveTransactions(updated);
  };

  const totalIncome = transactions
    .filter((t) => t.type === "income")
    .reduce((acc, t) => acc + t.amount, 0);

  const totalExpense = transactions
    .filter((t) => t.type === "expense")
    .reduce((acc, t) => acc + t.amount, 0);

  const balance = totalIncome - totalExpense;

  return (
    <main className="min-h-screen p-8 bg-gray-50">
      <div className="max-w-4xl mx-auto space-y-8">
        <div className="text-center space-y-2">
          <h1 className="text-4xl font-bold text-gray-900">Tableau de Bord</h1>
          <p className="text-gray-600">Suivez vos finances personnelles</p>
        </div>

        {/* Résumé */}
        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          <div className="bg-white p-6 rounded-xl shadow-sm border border-gray-200">
            <p className="text-sm text-gray-500">Solde</p>
            <p className={`text-2xl font-bold ${balance >= 0 ? 'text-green-600' : 'text-red-600'}`}>
              {balance.toFixed(2)} €
            </p>
          </div>
          <div className="bg-white p-6 rounded-xl shadow-sm border border-gray-200">
            <p className="text-sm text-gray-500">Revenus</p>
            <p className="text-2xl font-bold text-green-600">
              {totalIncome.toFixed(2)} €
            </p>
          </div>
          <div className="bg-white p-6 rounded-xl shadow-sm border border-gray-200">
            <p className="text-sm text-gray-500">Dépenses</p>
            <p className="text-2xl font-bold text-red-600">
              {totalExpense.toFixed(2)} €
            </p>
          </div>
        </div>

        {/* Formulaire d'ajout */}
        <AddTransaction onAdd={addTransaction} />

        {/* Liste des transactions */}
        <TransactionTable
          transactions={transactions}
          onDelete={deleteTransaction}
        />
      </div>
    </main>
  );
}