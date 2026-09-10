# F15 — Debug mode setting controls no behavior

Recommend removal; high confidence, medium/low priority. Owner C19, worker C19-01 ../workers/C19-WEB-SETTINGS.md. Independently reopened AboutSection, SettingsSheet preference initialization/write/diagnostics and prefs key. Repeated searches: no debug-key/flag consumer outside settings; all5 client console logging sites are unconditional and do not read it.

Settings explicitly promises verbose client logging, but toggling Debug mode only persists hirsel.debug and changes the copied diagnostics field. No logger or host consumes it. This is a shipped nonfunctional control plus orphaned persistent state, not an unimplemented hypothetical future feature.

Target delete the control, prop plumbing, signal/storage key and claimed diagnostics field. Preserve actual host debug configuration and all real diagnostics/device-label behavior. Do not build a logging framework to justify the control. Old local browser key may remain ignored; no migration/network/schema change. Scope AboutSection, SettingsSheet, prefs and relevant settings assertions. Validate the bogus control/field are absent and real diagnostics still copy correctly. Audit ran no tests.

Independent materiality priority: low. This is a bounded display/wholehog cleanup correction, not peer severity to durable admission or captured identity defects.
