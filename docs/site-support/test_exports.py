import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location("exports", Path(__file__).with_name("exports.py"))
exports = importlib.util.module_from_spec(spec)
spec.loader.exec_module(exports)


class ExportTests(unittest.TestCase):
    def test_nav_allowlist_filters_unpublished_markdown(self):
        files = [SimpleNamespace(src_uri=p, is_documentation_page=lambda p=p: p.endswith('.md')) for p in ['README.md', 'old-plan.md', 'img/card.png']]
        config = {'nav': [{'Start': 'README.md'}]}
        result = exports.on_files(files, config)
        self.assertEqual([f.src_uri for f in result], ['README.md', 'img/card.png'])

    def test_nested_navigation_preserves_publication_order_without_duplicates(self):
        nav = [{'Start': 'README.md'}, {'Guide': [{'Chat': 'guide/chat.md'}, {'Home': 'README.md'}]}]
        self.assertEqual(exports.published_paths(nav), ['README.md', 'guide/chat.md'])

    def test_build_configuration_reuses_the_repository_version_and_commit(self):
        config = {}
        exports.on_config(config)
        build = config['extra']['build']
        self.assertRegex(build['version'], r'^\d+\.\d+\.\d+$')
        self.assertRegex(build['commit'], r'^[0-9a-f]{40}$')
        self.assertIn(build['commit'][:12], config['copyright'])

    def test_foreign_git_environment_does_not_change_documentation_identity(self):
        baseline = {}
        exports.on_config(baseline)
        with tempfile.TemporaryDirectory() as tmp:
            with patch.dict('os.environ', {'GIT_DIR': str(Path(tmp) / 'missing-repository'), 'GIT_WORK_TREE': tmp, 'GIT_INDEX_FILE': str(Path(tmp) / 'foreign-index')}):
                actual = {}
                exports.on_config(actual)
        self.assertEqual(actual['extra']['build'], baseline['extra']['build'])

    def test_export_contains_exact_published_sources_and_build_identity(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            docs = root / 'docs'
            site = root / 'site'
            docs.mkdir()
            site.mkdir()
            (docs / 'README.md').write_text('# Hello\n\nExact canonical body.\n')
            (docs / 'secret-plan.md').write_text('Unpublished plan')
            (docs / 'img').mkdir()
            (docs / 'img/card.png').write_bytes(b'example-image')
            config = {'nav': [{'Start': 'README.md'}], 'docs_dir': str(docs), 'site_dir': str(site), 'extra': {'build': {'version': '1.2.3', 'commit': 'abc123'}}}
            exports.on_post_build(config)
            self.assertEqual((site / 'markdown/README.md').read_text(), (docs / 'README.md').read_text())
            self.assertIn('markdown/README.md', (site / 'llms.txt').read_text())
            self.assertIn('Exact canonical body.', (site / 'llms-full.txt').read_text())
            self.assertNotIn('Unpublished plan', (site / 'llms-full.txt').read_text())
            self.assertEqual(json.loads((site / 'build-info.json').read_text())['commit'], 'abc123')
            self.assertEqual((site / 'markdown/img/card.png').read_bytes(), b'example-image')

    def test_exported_source_references_are_navigable_without_changing_guide_links(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            docs = root / 'docs'
            site = root / 'site'
            (docs / 'guide').mkdir(parents=True)
            (docs / 'site-support').mkdir()
            site.mkdir()
            (docs / 'legacy.md').write_text('# Legacy')
            (docs / 'site-support/check.py').write_text('source')
            (docs / 'guide/chat.md').write_text('# Chat\n[Code](../../crates/example.rs)\n[Legacy](../legacy.md)\n[Checker](../site-support/check.py)\n[Guide](start.md)\n![Card](../img/card.png)\n')
            (docs / 'guide/start.md').write_text('# Start')
            config = {'nav': [{'Chat': 'guide/chat.md'}, {'Start': 'guide/start.md'}], 'docs_dir': str(docs), 'site_dir': str(site), 'extra': {'build': {'version': '1.2.3', 'commit': 'abc123'}}}
            exports.on_post_build(config)
            text = (site / 'markdown/guide/chat.md').read_text()
            self.assertIn('https://github.com/croit/aiplane/blob/abc123/crates/example.rs', text)
            self.assertIn('https://github.com/croit/aiplane/blob/abc123/docs/legacy.md', text)
            self.assertIn('https://github.com/croit/aiplane/blob/abc123/docs/site-support/check.py', text)
            self.assertIn('[Guide](start.md)', text)
            self.assertIn('![Card](../img/card.png)', text)
            self.assertIn(text, (site / 'llms-full.txt').read_text())

    def test_unpublished_document_links_point_to_repository_source(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'legacy.md').write_text('# Legacy')
            config = {'docs_dir': str(root), 'nav': [{'Start': 'README.md'}], 'extra': {'build': {'version': '1.2.3', 'commit': 'abc123'}}}
            page = SimpleNamespace(file=SimpleNamespace(src_uri='README.md'))
            result = exports.on_page_markdown('[Legacy](legacy.md#detail)', page, config, [])
            self.assertIn('https://github.com/croit/aiplane/blob/abc123/docs/legacy.md#detail', result)

    def test_source_links_are_pinned_only_in_rendered_markdown(self):
        page = SimpleNamespace(file=SimpleNamespace(src_uri='reference/api.md'))
        config = {'extra': {'build': {'version': '1.2.3', 'commit': 'abc123'}}}
        text = '[Code](../../crates/aiplane/src/main.rs) and [Guide](../README.md)'
        result = exports.on_page_markdown(text, page, config, [])
        self.assertIn('https://github.com/croit/aiplane/blob/abc123/crates/aiplane/src/main.rs', result)
        self.assertIn('[Guide](../README.md)', result)
        self.assertIn('1.2.3', result)


if __name__ == '__main__':
    unittest.main()
