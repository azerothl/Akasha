import { describe, expect, it } from 'vitest';
import { taskTitle, taskStatus } from './presentation';
describe('task presentation',()=>{
  it('hides already-truncated private prompts from old daemons',()=>{
    for(const text of ['[Composer mode: ask] Answer only…','[Task] Objective: [Orchestrated — disk deliverables REQUIRED]'])
      expect(taskTitle(text,'Demande')).toBe('Demande');
    expect(taskTitle('Comparer Vec et HashMap','Demande')).toBe('Comparer Vec et HashMap');
  });
  it('localizes statuses without changing unknown statuses',()=>{
    expect(taskStatus('RUNNING','fr')).toBe('En cours');
    expect(taskStatus('failed','en')).toBe('Failed');
    expect(taskStatus('custom','fr')).toBe('custom');
  });
});
