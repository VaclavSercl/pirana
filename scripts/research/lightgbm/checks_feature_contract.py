import hashlib,json,tempfile,unittest
from pathlib import Path
from book_dataset import FEATURES
from migrate_feature_contract import migrate
class MigrationTests(unittest.TestCase):
    def prepare(self,root):
        source=root/"source";source.mkdir()
        names=list(FEATURES);names[names.index("volatility_65_bps")]="volatility_60_bps"
        (source/"audit.json").write_text(json.dumps(dict(features=names,status="AUDITED_WITH_EXCLUSIONS")))
        (source/"observations.jsonl").write_bytes(b'{"fixture":1}\n')
        (source/"parser-used.py").write_bytes(b'# original recorded parser')
        (source/"parser-provenance.json").write_text(json.dumps(dict(sha256=hashlib.sha256((source/"parser-used.py").read_bytes()).hexdigest())))
        return source
    def test_rename_preserves_source_and_every_observation_byte(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);source=self.prepare(root);old=(source/"audit.json").read_bytes();dest=root/"new"
            result=migrate(source,dest)
            self.assertEqual(old,(source/"audit.json").read_bytes())
            self.assertEqual((source/"observations.jsonl").read_bytes(),(dest/"observations.jsonl").read_bytes())
            self.assertEqual(result["feature_contract"],2)
            self.assertFalse(result["contract_migration"]["numeric_transformation"])
            with self.assertRaises(FileExistsError):migrate(source,dest)
    def test_changed_parser_evidence_blocks_migration(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);source=self.prepare(root);(source/"parser-used.py").write_text('changed')
            with self.assertRaises(ValueError):migrate(source,root/"new")
            self.assertFalse((root/"new").exists())
if __name__=="__main__":unittest.main()
