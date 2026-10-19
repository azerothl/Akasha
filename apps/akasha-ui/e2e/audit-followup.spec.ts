import { expect, test, type Page } from '@playwright/test';
import fs from 'node:fs/promises';
const rootId='00000000-0000-0000-0000-000000000001';
async function fixture(page:Page, locale='fr', mode='simple', existing=false) {
  await page.addInitScript(({locale,mode,existing})=>{
    localStorage.setItem('akasha_setup_wizard_done','1');
    localStorage.setItem('akasha_onboarding_dismissed','1');
    localStorage.setItem('akasha_locale',locale);localStorage.setItem('akasha_ui_mode',mode);
    if(existing)localStorage.setItem('akasha_task_sidebar_open','1');
  },{locale,mode,existing});
  await page.route('**/__e2e_daemon/**',async route=>{
    const url=new URL(route.request().url());const p=url.pathname.replace('/__e2e_daemon','');
    let json:unknown={};
    if(p==='/'||p==='/api/status')json={status:'ok'};
    else if(p==='/api/tasks')json={tasks:[{id:rootId,status:'running',label:'[Composer mode: ask] private prompt…',created_at:new Date().toISOString()}]};
    else if(p==='/api/notes')json={notes:[]};
    else if(p==='/api/tools/policy')json={policy:{web_search_enabled:true}};
    else if(p==='/api/research/deep/start'){await route.fulfill({status:400,json:{error:'missing_topic'}});return;}
    await route.fulfill({status:200,json});
  });
}
for(const locale of ['fr','en'])for(const mode of ['simple','expert'])for(const existing of [false,true]){
  test(`navigation and content: ${locale}/${mode}/${existing?'existing':'new'}`,async({page})=>{
    await fixture(page,locale,mode,existing);await page.goto('/');
    await expect(page.getByRole('region',{name:locale==='fr'?'Travail actif':'Active work'})).toBeVisible();
    await expect(page.getByText('[Composer mode: ask] private prompt…',{exact:true})).toHaveCount(0);
    if(mode==='simple'){
      await expect(page.locator('#subagents-detail')).toHaveCount(0);
      await page.getByText(locale==='fr'?'Plus d’outils':'More tools',{exact:true}).press('Enter');
    }
    const ids=['chat','compare','research','cookbook','notes','tasks','scheduled','router','mission','calendar','memory','docs','settings'];
    for(const id of ids) await expect(page.locator('#tab-'+id)).toBeVisible();
    await page.locator('#chat-input').fill('Brouillon conservé\n  indentation');
    for(const id of ['research','notes','calendar','chat']){
      await page.locator('#tab-'+id).press('Enter');
      await expect(page.locator('#tab-'+id)).toHaveAttribute('aria-selected','true');
    }
    await expect(page.locator('#chat-input')).toHaveValue('Brouillon conservé\n  indentation');
    expect(await page.evaluate(()=>localStorage.getItem('akasha_task_sidebar_open'))).toBe(existing?'1':null);
    await page.locator('#tab-tasks').press('Enter');
    if(mode==='simple'){
      await expect(page.locator('.activity-panel-body')).toHaveCount(0);
      await page.getByRole('button',{name:locale==='fr'?'Afficher les détails techniques':'Show technical details',exact:true}).press('Enter');
    }
    await expect(page.locator('.activity-panel-body')).toBeVisible();
  });
}

for(const touch of [false,true])for(const width of [400,800,1280]){
  test(`target sizes ${touch?'coarse':'fine'} ${width}`,async({browser})=>{
    const context=await browser.newContext({viewport:{width,height:width===1280?720:600},hasTouch:touch});
    const page=await context.newPage();await fixture(page);await page.goto('/');
    const min=touch?44:32;
    await expect.poll(()=>page.evaluate(()=>matchMedia('(pointer:coarse)').matches)).toBe(touch);
    const help=page.getByRole('button',{name:'Aide : Chat',exact:true});
    const dual=page.getByRole('button',{name:'Double panneau',exact:true});
    const measurements=[];
    for(const button of [help,dual]){
      await expect(button).toBeVisible();const box=await button.boundingBox();
      expect(box!.width).toBeGreaterThanOrEqual(min);expect(box!.height).toBeGreaterThanOrEqual(min);
      await button.focus();await button.press('ArrowRight');
      expect(await button.evaluate(e=>getComputedStyle(e).outlineStyle)).toBe('solid');
      await button.hover();
      measurements.push({name:await button.innerText(),...box});
    }
    await dual.press('Space');await expect(dual).toHaveAttribute('aria-pressed','true');
    await dual.press('Space');await expect(dual).toHaveAttribute('aria-pressed','false');
    if(touch){await dual.tap();await expect(dual).toHaveAttribute('aria-pressed','true');await dual.tap();}
    if(width===400) await page.getByRole('button',{name:'Ouvrir la navigation',exact:true}).click();
    const collapse=page.getByRole('button',{name:'Replier la barre latérale',exact:true});
    await expect.poll(()=>collapse.evaluate(e=>e.closest('.sidebar-left')!.getBoundingClientRect().left)).toBe(0);
    await expect(collapse).toBeVisible();const box=await collapse.boundingBox();
    expect(box!.width).toBeGreaterThanOrEqual(min);expect(box!.height).toBeGreaterThanOrEqual(min);
    await collapse.focus();await collapse.press('ArrowRight');
    expect(await collapse.evaluate(e=>getComputedStyle(e).outlineStyle)).toBe('solid');
    measurements.push({name:'sidebar collapse',...box});
    expect(await collapse.evaluate(e=>{const r=e.getBoundingClientRect();const hit=document.elementFromPoint(r.x+r.width/2,r.y+r.height/2);return !!hit&&(hit===e||e.contains(hit));})).toBe(true);
    await fs.mkdir('test-results',{recursive:true});
    await fs.writeFile(`test-results/targets-${touch?'touch':'desktop'}-${width}.json`,JSON.stringify({engine:browser.version(),emulatedTouch:touch,width,measurements},null,2));
    await page.screenshot({path:`test-results/targets-${touch?'touch':'desktop'}-${width}.png`});
    await context.close();
  });
}

for(const outcome of ['ready','limited','empty','error','cancelled'] as const){
  test(`research outcome: ${outcome}`,async({page})=>{
    await fixture(page);let polls=0;
    await page.route('**/__e2e_daemon/api/research/deep/start',async route=>{
      const data=route.request().postDataJSON();await route.fulfill(data.topic?{json:{run_id:'dr_fixture'}}:{status:400,json:{error:'missing_topic'}});
    });
    await page.route('**/__e2e_daemon/api/research/deep/runs/dr_fixture',async route=>{
      polls++;
      const phase=polls===1?'searching':outcome==='error'?'error':outcome==='cancelled'?'cancelled':'done';
      const sources=outcome==='empty'?[]:[{title:'Rust documentation',url:'https://doc.rust-lang.org/std/collections/'}];
      await route.fulfill({json:{id:'dr_fixture',topic:'Rust collections',phase,round:1,max_rounds:1,
        branches:[],evolving_report:'',search_degraded:false,error:phase==='error'?'research_no_evidence':null,
        report_markdown:phase==='done'?(outcome==='empty'?'No information could be gathered for this question.':'# Collections\n\nVec est une séquence contiguë. [Documentation Rust](https://doc.rust-lang.org/std/collections/).'):null,
        report_meta:{sources,sources_sufficient:outcome==='ready',quality_warnings:[],search_degraded:false,category:'general',word_count:20,rounds:1,duration_secs:1,providers_used:['fixture']}}});
    });
    await page.goto('/#/research');const topic=page.locator('.research-content textarea');
    const run=page.getByRole('button',{name:'Lancer la recherche',exact:true});
    await topic.fill('   ');await expect(run).toBeDisabled();await topic.fill('Rust collections');await run.click();
    await expect(page.getByRole('button',{name:'Annuler',exact:true})).toBeVisible();
    if(outcome==='ready'||outcome==='limited'){
      await expect(page.getByRole('region',{name:'Rapport de recherche'})).toContainText('Vec est une séquence contiguë');
      if(outcome==='limited')await expect(page.getByText(/Rapport partiel/)).toBeVisible();
    }else if(outcome==='empty'||outcome==='error'){
      await expect(page.locator('.research-result-notice[role="alert"]')).toContainText('Aucune source exploitable');
      await expect(page.getByRole('button',{name:'Rouvrir le rapport'})).toHaveCount(0);
    }else{await expect(run).toBeEnabled();}
    await page.screenshot({path:`test-results/research-${outcome}.png`});
  });
}

test('failed deferred chunk keeps chat draft and offers keyboard recovery',async({page})=>{
  await fixture(page);await page.goto('/');await page.locator('#chat-input').fill('Brouillon protégé');
  await page.route('**/assets/DeepResearchPanel-*.js',r=>r.abort());
  await page.locator('#tab-research').press('Enter');
  await expect(page.getByRole('alert')).toContainText('Ce panneau n’a pas pu être chargé');
  await page.getByRole('button',{name:'Retour au Chat',exact:true}).press('Enter');
  await expect(page.locator('#chat-input')).toHaveValue('Brouillon protégé');
});

for (const mode of ['simple','expert']) {
  test(`task sidebar respects ${mode} mode`, async ({page}) => {
    await page.setViewportSize({width:1280,height:720});
    await fixture(page,'fr',mode,true);
    await page.route('**/__e2e_daemon/api/tasks', route=>route.fulfill({json:{tasks:[
      {id:rootId,label:'Demande principale',status:'waiting_user_input',assigned_agent:'private-root-agent'},
      {id:'00000000-0000-0000-0000-000000000002',parent_task_id:rootId,label:'Sous-tâche interne',status:'running',assigned_agent:'private-child-agent'},
    ]}}));
    await page.goto('/');
    if(mode==='simple') await page.getByText('Plus d’outils',{exact:true}).click();
    await page.locator('#tab-tasks').click();
    const sidebar=page.locator('.sidebar-right-task-list');
    await expect(sidebar.getByText('Demande principale',{exact:true})).toBeVisible();
    await expect(sidebar.locator('.sidebar-right-task-card')).toHaveCount(mode==='simple'?1:2);
    await expect(sidebar.getByText('Sous-tâche interne',{exact:true})).toHaveCount(mode==='simple'?0:1);
    await expect(sidebar.locator('.sidebar-right-task-agent')).toHaveCount(mode==='simple'?0:2);
    await expect(sidebar.locator('.task-tree-meta-row')).toHaveCount(mode==='simple'?0:2);
    await expect(sidebar.locator('.sidebar-right-task-status-pill').first()).toHaveText('Votre réponse est attendue');
    if(mode==='simple') {
      await expect(sidebar.locator('.sidebar-right-task-snippet,.sidebar-right-task-progress,.task-tree-toggle')).toHaveCount(0);
      await expect(page.locator('.task-tree-toolbar-sidebar')).toHaveCount(0);
      await expect(page.locator('.task-center-chip-running')).toContainText('1');
    } else {
      await sidebar.locator('.sidebar-right-task-card-inner').filter({hasText:'Sous-tâche interne'}).click();
      await expect(page.locator('.task-center-selected-title')).toHaveText('Sous-tâche interne');
      await page.locator('#tab-settings').click();
      await page.locator('select').filter({has:page.locator('option[value="simple"]')}).selectOption('simple');
      await page.getByText('Plus d’outils',{exact:true}).click();
      await page.locator('#tab-tasks').click();
      await expect(page.locator('.task-center-selected-title')).toHaveText('Demande principale');
      await expect(page.getByText('Sous-tâche interne',{exact:true})).toHaveCount(0);
      await expect(sidebar.locator('.sidebar-right-task-card.selected')).toHaveCount(1);
    }
  });
}

for (const resume of [false,true]) {
  test(`failed research preserves terminal diagnostic history: ${resume?'resumed':'new'}`, async ({page}) => {
    await fixture(page);
    if(resume) await page.addInitScript(()=>localStorage.setItem('akasha_deep_research_v2',JSON.stringify([{
      id:'res_existing',serverRunId:'dr_diagnostics',topic:'Diagnostic test',maxRounds:1,
      createdAt:new Date().toISOString(),updatedAt:new Date().toISOString(),status:'running',branches:[],
    }])));
    await page.route('**/__e2e_daemon/api/research/deep/start',async route=>{
      await route.fulfill(route.request().postDataJSON().topic?{json:{run_id:'dr_diagnostics'}}:{status:400,json:{error:'missing_topic'}});
    });
    await page.route('**/__e2e_daemon/api/research/deep/runs/dr_diagnostics',route=>route.fulfill({json:{
      id:'dr_diagnostics',topic:'Diagnostic test',phase:'error',round:1,max_rounds:1,
      branches:[{id:'round_1',label:'Failed round',status:'error',sub_agents:[{id:'analyst_1',role:'analyst',label:'Unavailable model',status:'error'}]}],
      evolving_report:'Intermediate diagnostic notes',search_degraded:true,error:'research_no_evidence',report_markdown:null,
    }}));
    await page.goto('/#/research');
    if(!resume) {
      await page.locator('.research-content textarea').fill('Diagnostic test');
      await page.getByRole('button',{name:'Lancer la recherche',exact:true}).click();
    }
    await expect(page.locator('.research-result-notice[role="alert"]')).toContainText('Aucune source exploitable');
    await expect.poll(()=>page.evaluate(()=>{
      const row=JSON.parse(localStorage.getItem('akasha_deep_research_v2')??'[]')[0];
      return {status:row?.status,degraded:row?.searchDegraded,notes:row?.evolvingReport,branch:row?.branches?.[0]?.status,agent:row?.branches?.[0]?.subAgents?.[0]?.status};
    })).toEqual({status:'error',degraded:true,notes:'Intermediate diagnostic notes',branch:'error',agent:'error'});
    await page.reload();
    await expect(page.locator('.research-result-notice[role="alert"]')).toContainText('Aucune source exploitable');
    expect(await page.evaluate(()=>JSON.parse(localStorage.getItem('akasha_deep_research_v2')??'[]')[0]?.branches?.[0]?.subAgents?.[0]?.status)).toBe('error');
  });
}
