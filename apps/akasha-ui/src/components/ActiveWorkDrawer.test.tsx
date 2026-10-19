// @vitest-environment jsdom
import { afterEach, expect, it } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { ActiveWorkDrawer } from './ActiveWorkDrawer';
afterEach(cleanup);
const tasks=[{id:'root',status:'running',label:'Comparer des collections'},
  {id:'child',parent_task_id:'root',status:'running',label:'[Orchestrated — disk deliverables REQUIRED] secret instruction'}];
it('shows one user request and localized state in Simple',()=>{
  render(<ActiveWorkDrawer locale="fr" simple tasks={tasks} onOpenTask={()=>{}} onCancel={()=>{}} />);
  expect(screen.getByText('Comparer des collections')).toBeTruthy();
  expect(screen.getByText('En cours')).toBeTruthy();
  expect(screen.queryByText(/Inspecter/)).toBeNull();
  expect(screen.queryByText(/secret instruction/)).toBeNull();
});
it('keeps the child controls accessible in Expert',()=>{
  render(<ActiveWorkDrawer locale="en" simple={false} tasks={tasks} onOpenTask={()=>{}} onCancel={()=>{}} />);
  expect(screen.getByRole('button',{name:'Inspect'})).toBeTruthy();
  expect(screen.getAllByText('Running')).toHaveLength(2);
});
