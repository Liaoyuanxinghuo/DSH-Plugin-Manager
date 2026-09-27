# -*- coding: utf-8 -*-
# types.ts
p = 'E:/workspace/dshcjaz/src/types.ts'
s = open(p, encoding='utf-8').read()

old = '''export interface ProfileInfo {
  name: string;
  path: string;'''
new = '''export interface ProfileInfo {
  name: string;
  /** 所在 profiles 目录（来源），任意 dsh × 任意来源 profile 组合启动时注入 DSH_HOME */
  profilesDir: string;
  path: string;'''
assert old in s, 'ProfileInfo 未找到'
s = s.replace(old, new, 1)

old2 = '''export interface RunningProcess {
  envId: string;
  profile: string;'''
new2 = '''export interface RunningProcess {
  envId: string;
  /** profile 来源目录 */
  profilesDir: string;
  profile: string;'''
assert old2 in s, 'RunningProcess 未找到'
s = s.replace(old2, new2, 1)
open(p, 'w', encoding='utf-8', newline='').write(s)
print('types.ts OK')

# api.ts
p2 = 'E:/workspace/dshcjaz/src/api.ts'
s2 = open(p2, encoding='utf-8').read()

repls = [
    ('  listProfiles: (envId: string) =>',
     '  listAllProfiles: () => invoke<ProfileInfo[]>("list_all_profiles"),\n  listProfiles: (envId: string) =>'),
    ('  listPlugins: (envId: string, profile: string) =>',
     '  listPlugins: (envId: string, profile: string, profilesDir: string) =>'),
    ('  setPluginEnabled: (envId: string, profile: string, packageName: string, enabled: boolean) =>',
     '  setPluginEnabled: (envId: string, profile: string, profilesDir: string, packageName: string, enabled: boolean) =>'),
    ('  checkUpdates: (envId: string, profile: string) =>',
     '  checkUpdates: (envId: string, profile: string, profilesDir: string) =>'),
    ('  startDsh: (envId: string, profile: string, port?: number) =>',
     '  startDsh: (envId: string, profile: string, profilesDir: string, port?: number) =>'),
    ('  stopDsh: (envId: string, profile: string) =>',
     '  stopDsh: (envId: string, profile: string, profilesDir: string) =>'),
    ('  dshStatus: (envId: string, profile: string) =>',
     '  dshStatus: (envId: string, profile: string, profilesDir: string) =>'),
    ('  createProfile: (envId: string, name: string) =>',
     '  createProfile: (envId: string, name: string, profilesDir: string) =>'),
    ('  deleteProfile: (envId: string, name: string) =>',
     '  deleteProfile: (envId: string, name: string, profilesDir: string) =>'),
    ('  checkDeps: (envId: string, profile: string) =>',
     '  checkDeps: (envId: string, profile: string, profilesDir: string) =>'),
    ('  fixDeps: (envId: string, profile: string) =>',
     '  fixDeps: (envId: string, profile: string, profilesDir: string) =>'),
    ('  scanJunk: (envId: string, profile: string) =>',
     '  scanJunk: (envId: string, profile: string, profilesDir: string) =>'),
    ('  cleanJunk: (envId: string, profile: string, names: string[]) =>',
     '  cleanJunk: (envId: string, profile: string, profilesDir: string, names: string[]) =>'),
    ('  exportProfile: (envId: string, profile: string, targetPath: string, excludeNodeModules?: boolean) =>',
     '  exportProfile: (envId: string, profile: string, profilesDir: string, targetPath: string, excludeNodeModules?: boolean) =>'),
    ('  importProfile: (envId: string, zipPath: string) =>',
     '  importProfile: (envId: string, zipPath: string, profilesDir: string) =>'),
    ('  profileNodeModulesSize: (envId: string, profile: string) =>',
     '  profileNodeModulesSize: (envId: string, profile: string, profilesDir: string) =>'),
]
ok = 0
for old, new in repls:
    if old in s2:
        s2 = s2.replace(old, new, 1)
        ok += 1
    else:
        print('api 未匹配:', old)
print('api 替换:', ok, '/', len(repls))
open(p2, 'w', encoding='utf-8', newline='').write(s2)
print('api.ts OK')
