import { defineConfig } from '@playwright/test';
import base from './playwright.config';
/** Deterministic UI integration fixtures; no daemon, models or user data. */
export default defineConfig({...base,globalSetup:undefined,globalTeardown:undefined,
  testMatch:'audit-followup.spec.ts',use:{...base.use,baseURL:'http://127.0.0.1:4189'},
  webServer:{command:'node scripts/e2e-preview.mjs',url:'http://127.0.0.1:4189',
    env:{...process.env,HOST:'127.0.0.1',PORT:'4189'},reuseExistingServer:false},
});
