# -*- coding: utf-8 -*-
# api.ts：invoke 对象里简写 profilesDir → profilesDirStr: profilesDir（Tauri 映射 profiles_dir_str）
p = 'E:/workspace/dshcjaz/src/api.ts'
s = open(p, encoding='utf-8').read()
reps = [
('{ envId, profile, profilesDir }',
 '{ envId, profile, profilesDirStr: profilesDir }'),
('{ envId, profile, profilesDir, packageName, enabled }',
 '{ envId, profile, profilesDirStr: profilesDir, packageName, enabled }'),
('{ envId, profile, profilesDir, name }',
 '{ envId, profile, profilesDirStr: profilesDir, name }'),
('{ envId, profile, profilesDir, targetPath, excludeNodeModules }',
 '{ envId, profile, profilesDirStr: profilesDir, targetPath, excludeNodeModules }'),
('{ envId, zipPath, profilesDir }',
 '{ envId, zipPath, profilesDirStr: profilesDir }'),
('{ envId, name, profilesDir }',
 '{ envId, name, profilesDirStr: profilesDir }'),
('{ envId, profile, profilesDir, names }',
 '{ envId, profile, profilesDirStr: profilesDir, names }'),
]
ok = 0
for old, new in reps:
    n = s.count(old)
    s = s.replace(old, new)
    ok += n
print('api.ts 替换次数:', ok)
open(p, 'w', encoding='utf-8', newline='').write(s)

# App.test.tsx 断言：profilesDir: → profilesDirStr:
p2 = 'E:/workspace/dshcjaz/src/App.test.tsx'
s2 = open(p2, encoding='utf-8').read()
# 只替换断言对象里的 key（形如  profilesDir: " 或  profilesDir: "" 的行），不替换 mock 数据里的 profilesDir: "C:\Users\test\.dsh\profiles"
# mock 数据里的 profilesDir 是 ProfileInfo 字段（camelCase 序列化）——保留！
# 断言里的 key 也写 profilesDir（原样）——但那是 invoke 参数 key，应为 profilesDirStr。
# 区分：invoke 断言形如  toHaveBeenCalledWith("xxx_cmd", { ... profilesDir: "C:\\Users\\test\\.dsh\\profiles" ... })
# mock 数据形如  { name: "web", profilesDir: "C:\\Users\\test\\...", path: ... } （list_all_profiles 返回值）
# 用正则区分：断言块里 profilesDir 出现在 { envId, ... } 对象内；mock 里出现在 { name: ... } 内。
import re
# 断言对象：以 envId 开头到 } 结束的块里的 profilesDir → profilesDirStr
def fix_assert(m):
    return m.group(0).replace('profilesDir', 'profilesDirStr')
s2, n2 = re.subn(r'(\{\s*envId:[^\n]*\n(?:[^\n]*\n)*?[^\n]*\})', fix_assert, s2)
# 单行断言（create_profile：{ envId: "global", name: "dev", profilesDir: "" }）
s2 = s2.replace('{ envId: "global", name: "dev", profilesDir: "" }',
                '{ envId: "global", name: "dev", profilesDirStr: "" }')
print('test 断言替换块数:', n2)
open(p2, 'w', encoding='utf-8', newline='').write(s2)
print('OK')
