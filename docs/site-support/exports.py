"""MkDocs hooks for publication from a single canonical Markdown source."""

import json
import os
from pathlib import Path
import posixpath
import re
import shutil
import subprocess


def published_paths(nav):
    result = []
    for item in nav:
        if isinstance(item, str):
            if item.endswith('.md'):
                result.append(item)
        elif isinstance(item, dict):
            for value in item.values():
                result.extend(published_paths(value if isinstance(value, list) else [value]))
    return list(dict.fromkeys(result))


def on_config(config):
    inherited_git_vars = {
        'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_OBJECT_DIRECTORY',
        'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_COMMON_DIR', 'GIT_NAMESPACE',
        'GIT_PREFIX',
    }
    repository_env = {name: value for name, value in os.environ.items() if name not in inherited_git_vars}
    version = subprocess.check_output(['sh', 'scripts/derive-version.sh'], text=True, env=repository_env).strip()
    commit = os.environ.get('CI_COMMIT_SHA') or os.environ.get('GIT_SHA') or subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True, env=repository_env).strip()
    dirty = bool(subprocess.check_output(['git', 'status', '--porcelain'], text=True, env=repository_env).strip())
    config.setdefault('extra', {})['build'] = {'version': version, 'commit': commit, 'dirty': dirty}
    config['copyright'] = f'croit AIplane {version} · commit {commit[:12]}' + (' · working tree changes' if dirty else '')
    return config


def on_files(files, config):
    published = set(published_paths(config['nav']))
    for file in list(files):
        if file.src_uri.startswith('site-support/') or (file.is_documentation_page() and file.src_uri not in published):
            files.remove(file)
    return files


def rewrite_source_links(markdown, source_uri, config):
    build = config['extra']['build']
    base = posixpath.dirname(source_uri)
    published = set(published_paths(config.get('nav', [])))

    def source_link(match):
        target = match.group(1)
        if ':' in target or target.startswith(('#', '/')):
            return match.group(0)
        resolved = posixpath.normpath(posixpath.join(base, target))
        if resolved.startswith('../'):
            resolved = resolved.removeprefix('../')
            return f'](https://github.com/croit/aiplane/blob/{build["commit"]}/{resolved})'
        source_path = resolved.split('#', 1)[0]
        if (source_path.startswith('site-support/') or (source_path.endswith('.md') and source_path not in published)) and config.get('docs_dir') and (Path(config['docs_dir']) / source_path).is_file():
            return f'](https://github.com/croit/aiplane/blob/{build["commit"]}/docs/{resolved})'
        return match.group(0)

    return re.sub(r'\]\(([^\s)]+)\)', source_link, markdown)


def on_page_markdown(markdown, page, config, files):
    build = config['extra']['build']
    markdown = rewrite_source_links(markdown, page.file.src_uri, config)
    stamp = f'Documentation build: `{build["version"]}` · commit `{build["commit"][:12]}`'
    if build.get('dirty'):
        stamp += ' · working tree changes'
    return markdown + '\n\n---\n\n' + stamp + '\n'


def on_post_build(config):
    docs = Path(config['docs_dir'])
    site = Path(config['site_dir'])
    build = config['extra']['build']
    index = ['# croit AIplane', '', '> Documentation for the application build identified below.', '',
             f'Version: {build["version"]}', f'Commit: {build["commit"]}',
             f'Working tree changes: {str(build.get("dirty", False)).lower()}', '',
             'Links are relative to this documentation directory, on GitHub Pages or at /docs/ in an installation.', '',
             '## Documentation', '']
    full = index[:9] + ['']
    for rel in published_paths(config['nav']):
        text = rewrite_source_links((docs / rel).read_text(encoding='utf-8'), rel, config)
        title = next((line.removeprefix('# ').strip() for line in text.splitlines() if line.startswith('# ')), rel)
        destination = site / 'markdown' / rel
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(text, encoding='utf-8')
        index.append(f'- [{title}](markdown/{rel})')
        full.extend([f'<!-- Source: markdown/{rel} -->', f'<!-- Resolve relative links in this section against markdown/{rel}. -->', text, '\n---\n'])
    for asset in docs.rglob('*'):
        if asset.is_file() and asset.suffix != '.md' and 'site-support' not in asset.relative_to(docs).parts:
            destination = site / 'markdown' / asset.relative_to(docs)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(asset, destination)
    index.extend(['', '## Complete text', '', '- [All published documentation](llms-full.txt)', '- [Build identity](build-info.json)', ''])
    (site / 'llms.txt').write_text('\n'.join(index), encoding='utf-8')
    (site / 'llms-full.txt').write_text('\n'.join(full), encoding='utf-8')
    (site / 'build-info.json').write_text(json.dumps(build, indent=2) + '\n', encoding='utf-8')
