"""Check original quotations against independently fetched HTML, not semantics.
Usage: python audit-source-quotes.py DATA_DIR
Requires run-final.json, source-checks.json, source-0.html ... source-3.html.
The source HTML is retained locally; hashes are versioned with the evidence.
"""
from pathlib import Path
from html.parser import HTMLParser
import hashlib, json, re, sys

base = Path(sys.argv[1])
run = json.loads((base / 'run-final.json').read_text(encoding='utf-8'))
if run['phase'] != 'done' or not run.get('report_markdown'):
    raise SystemExit('Run not complete: no positive validation')
report = run['report_markdown']
checks = json.loads((base / 'source-checks.json').read_text(encoding='utf-8'))

class Text(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.parts = []
    def handle_data(self, data):
        self.parts.append(data)

def normalize(value):
    return ''.join(value.split())

def unescape(value):
    return re.sub(r'\\([\\`*_\[\]<>|])', r'\1', value)

sources = {}
for index, check in enumerate(checks):
    body = (base / f'source-{index}.html').read_bytes()
    if hashlib.sha256(body).hexdigest() != check['sha256']:
        raise SystemExit('Independent source snapshot hash mismatch')
    parser = Text()
    parser.feed(body.decode('utf-8'))
    sources[check['url']] = normalize(' '.join(parser.parts))

violations = []
count = 0
pending = []
for line in report.splitlines():
    if line.startswith('> '):
        pending.append(unescape(line[2:]))
    elif pending and line.startswith('[Source](<'):
        url = line[len('[Source](<'):-2]
        if url not in sources:
            violations.append({'url': url, 'reason': 'unknown original source'})
        else:
            for quote in pending:
                if re.fullmatch(r'\|(?:-+\|)+', quote):
                    continue  # Markdown layout separator, no factual content.
                pieces = [cell.strip() for cell in quote.split('|') if cell.strip()] if quote.startswith('|') else [quote]
                for piece in pieces:
                    count += 1
                    if normalize(piece) not in sources[url]:
                        violations.append({'url': url, 'quote': piece})
        pending = []
if pending:
    violations.append({'reason': 'quotation has no attached source'})

plain = unescape(report)
case_checks = {
    'within_requested_word_maximum': len(report.split()) <= 400,
    'vec_original_distinct_push_pop_qualifiers': 'amortized O(1) push (to the end) and O(1) pop (from the end)' in plain,
    'vec_cost_row_present': '| Vec | O(1) | O(n-i)* | O(n-i) |' in plain,
    'vecdeque_cost_row_present': '| VecDeque | O(1) | O(min(i, n-i))* | O(min(i, n-i)) |' in plain,
    'size_not_capacity_definition': 'The collection’s size is denoted by n.' in plain,
    'amortization_legend_present': 'Operations which have an amortized cost are suffixed with a *.' in plain,
    'removal_qualification_present': 'removal operations aren’t amortized.' in plain,
    'original_quotations_present': count > 0,
}
result = {
    'run_id': run['id'], 'phase': run['phase'],
    'report_words': len(report.split()),
    'quoted_cells_or_passages_checked': count,
    'quotation_violations': violations,
    'case_checks': case_checks,
    'manual_free_prose_review_required': True,
}
(base / 'quote-audit.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
print(json.dumps(result, ensure_ascii=True, indent=2))
if violations or not all(case_checks.values()):
    raise SystemExit(1)
