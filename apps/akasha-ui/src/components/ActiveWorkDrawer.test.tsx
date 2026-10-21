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

it('hides orphaned internal work in Simple even when its parent is absent',()=>{
  const orphan={id:'orphan',parent_task_id:'inactive-or-outside-page',status:'running',label:'Private orphan work'};
  const {container,rerender}=render(<ActiveWorkDrawer locale="fr" simple tasks={[orphan]} onOpenTask={()=>{}} onCancel={()=>{}} />);
  expect(container.innerHTML).toBe('');
  rerender(<ActiveWorkDrawer locale="fr" simple tasks={[...tasks,orphan]} onOpenTask={()=>{}} onCancel={()=>{}} />);
  expect(screen.getByText('Comparer des collections')).toBeTruthy();
  expect(screen.getByText('(1)')).toBeTruthy();
  expect(screen.queryByText('Private orphan work')).toBeNull();
  rerender(<ActiveWorkDrawer locale="fr" simple={false} tasks={[orphan]} onOpenTask={()=>{}} onCancel={()=>{}} />);
  expect(screen.getByText('Private orphan work')).toBeTruthy();
});
