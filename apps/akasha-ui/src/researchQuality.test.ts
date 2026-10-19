import { expect, it } from 'vitest';
import { researchQuality } from './researchQuality';
import type { ResearchReportMeta } from './researchReportExport';
const meta:ResearchReportMeta={category:'general',word_count:100,sources:[{title:'Rust',url:'https://doc.rust-lang.org/'}],providers_used:['duckduckgo'],rounds:1,duration_secs:10,search_degraded:false,sources_sufficient:true};
it('rejects legacy empty-success history',()=>{
  expect(researchQuality('No information could be gathered for this question.')).toBe('empty');
  expect(researchQuality('Unverified claims',{...meta,sources:[]})).toBe('empty');
});
it('distinguishes a sourced report, partial evidence and offline knowledge',()=>{
  expect(researchQuality('A sourced report',meta)).toBe('ready');
  expect(researchQuality('Partial report',{...meta,sources_sufficient:false})).toBe('limited');
  expect(researchQuality('Model knowledge',{...meta,sources:[],search_degraded:true})).toBe('limited');
});

it('keeps direct page evidence independent from discovery availability',()=>{
  const direct={...meta,providers_used:[],search_degraded:true};
  expect(researchQuality('Direct source report',direct)).toBe('ready');
  expect(researchQuality('Thin direct report',{...direct,sources_sufficient:false})).toBe('limited');
  expect(researchQuality('Model notes',{...direct,sources:[]})).toBe('limited');
});
