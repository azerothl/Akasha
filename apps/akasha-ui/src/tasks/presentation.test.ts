import { describe, expect, it } from 'vitest';
import { taskTitle, taskStatus } from './presentation';
describe('task presentation',()=>{
  it('hides already-truncated private prompts from old daemons',()=>{
    for(const text of ['[Composer mode: ask] Answer only…','[Task] Objective: [Orchestrated — disk deliverables REQUIRED]'])
      expect(taskTitle(text,'Demande')).toBe('Demande');
    for (const text of ['Comparer Vec et HashMap','[Task] investigate outage','[Orchestrated architecture] explain layers','[Pièce(s) jointe] explain this term'])
      expect(taskTitle(text,'Demande')).toBe(text);
    for (const text of ['[Task]\nObjective: [Orchestrated — disk deliverables REQUIRED]', '[Orchestrated — disk deliverables REQUIRED] internal prompt…', '[Pièce(s) jointe(s) à ce message : aide] private attachment…'])
      expect(taskTitle(text,'Demande')).toBe('Demande');
  });
  it('localizes statuses without changing unknown statuses',()=>{
    expect(taskStatus('RUNNING','fr')).toBe('En cours');
    expect(taskStatus('failed','en')).toBe('Failed');
    for (const [state, fr, en] of [
      ['pending','En attente','Pending'], ['queued','En file d’attente','Queued'],
      ['running','En cours','Running'], ['completed','Terminée','Completed'],
      ['failed','Échec','Failed'], ['paused','En pause','Paused'],
      ['cancelled','Annulée','Cancelled'], ['waiting_user_input','Votre réponse est attendue','Waiting for your input'],
      ['interrupted','Interrompue','Interrupted'],
    ]) {
      expect(taskStatus(state,'fr')).toBe(fr);
      expect(taskStatus(state.toUpperCase(),'en')).toBe(en);
    }
    expect(taskStatus('custom','fr')).toBe('custom');
  });
});
