"""Compare native and Rust builders/readers on Chinese, Japanese and Korean ZIMs.

Requires zimru's zimwriterfs and zimdump, both builders, zimsearch, xapian-check,
python-libzim, and the xapianbuilder bench/upstream-helper.cc snapshot program.
Pass an empty output directory to retain the archives, snapshots and report.
"""

import argparse
import base64
import difflib
import json
import subprocess
from collections import Counter
from html import escape
from pathlib import Path

from libzim.reader import Archive
from libzim.search import Query, Searcher
from libzim.suggestion import SuggestionSearcher

# Each language has articles with overlapping terms and a title redirect.
# Include the native builder's 中国语言研究 bigram regression verbatim.
CASES = {
    "zho": {
        "docs": [
            (
                "中国语言研究",
                "中国语言研究",
                "中国语言研究，中国语言学。漢字與汉字，上海地铁。",
            ),
            ("上海", "上海地铁指南", "上海地铁和北京地铁。中国交通与中国语言研究。"),
            ("北京", "北京地铁指南", "中国北京地铁线路，地铁地铁。"),
            (
                "混合",
                "AI中国2026",
                "AI中国2026；ＡＩ、繁體中文、简体中文。罕见字𠀀野。",
            ),
        ],
        "alias": "上海轨道交通",
        "fulltext": [
            "中国语言研究",
            "国语",
            "语言",
            "言研",
            "研究",
            "地铁",
            "中国",
            "上海地铁",
            "轨道",
            "繁體中文",
            "AI中国2026",
            "ＡＩ",
            "𠀀野",
            "线路",
            "绝无匹配xyz",
        ],
        "suggest": [
            "中",
            "中国语",
            "上",
            "上海",
            "上海轨",
            "北京地",
            "AI中",
            "绝无匹配xyz",
        ],
        "multi": "地铁",
    },
    "jpn": {
        "docs": [
            (
                "東京",
                "東京大学の研究",
                "東京大学で日本語を研究する。東京、京都。がっこうとカタカナ。",
            ),
            ("鉄道", "東京の鉄道", "東京駅から京都駅へ。新幹線と東京の鉄道。"),
            ("かな", "がっこう案内", "か\u3099っこう、ガイド、ひらがな、カタカナ。"),
            ("混在", "AI東京2026", "AI東京2026。ｶﾀｶﾅと漢字、ひらがな。"),
        ],
        "alias": "東京鉄道案内",
        "fulltext": [
            "東京",
            "大学",
            "日本語",
            "京都駅",
            "新幹線",
            "カタカナ",
            "がっこう",
            "か\u3099っこう",
            "ひらがな",
            "ｶﾀｶﾅ",
            "AI東京2026",
            "絶対にないxyz",
        ],
        "suggest": [
            "東",
            "東京",
            "東京大",
            "東京鉄",
            "がっ",
            "か\u3099っ",
            "AI東",
            "絶対にないxyz",
        ],
        "multi": "東京",
    },
    "zho-extensions": {
        "docs": [
            ("ext-b", "常见与扩展", "共同测试。对照字符𠀀野。"),
            ("ext-g", "𰻞𰻞面", "共同测试。𰻞𰻞。"),
            ("ext-h", "𱍐𱍑字", "共同测试。𱍐𱍑。"),
            ("ext-i", "𮯰𮯱字", "共同测试。𮯰𮯱。"),
        ],
        "alias": "面条介绍",
        "fulltext": ["共同测试", "𠀀野", "𰻞𰻞", "𱍐𱍑", "𮯰𮯱", "绝无匹配xyz"],
        "suggest": ["常见", "面条", "𰻞𰻞", "𱍐𱍑", "𮯰𮯱", "绝无匹配xyz"],
        "multi": "共同测试",
    },
    "kor": {
        "docs": [
            (
                "서울",
                "서울 지하철 안내",
                "서울 지하철과 부산 지하철. 한글 연구와 한국어 교육.",
            ),
            ("부산", "부산 지하철 노선", "서울과 부산 사이의 철도. 부산 지하철 역사."),
            ("한글", "한글 연구소", "한글과 한글, 韓國語와 한자. 서울 연구소."),
            ("혼합", "AI서울2026", "AI서울2026, 한국어와 漢字, １２３."),
        ],
        "alias": "부산 철도 안내",
        "fulltext": [
            "서울",
            "지하철",
            "부산",
            "한국어",
            "한글",
            "한글",
            "韓國語",
            "한자",
            "연구",
            "AI서울2026",
            "１２３",
            "없는결과xyz",
        ],
        "suggest": [
            "서",
            "서울",
            "부산",
            "부산 철",
            "한글",
            "한글",
            "AI서",
            "없는결과xyz",
        ],
        "multi": "지하철",
    },
}


def output(*argv):
    return subprocess.check_output(argv)


def native_paths(archive, mode, query):
    if mode == "suggest":
        result = SuggestionSearcher(archive).suggest(query)
    else:
        result = Searcher(archive).search(Query().set_query(query))
    # python-libzim 3.x yields paths; 4.x yields SearchResult for full text.
    if hasattr(result, "get_results"):
        matches = result.get_results(0, 10)
    else:
        matches = result.getResults(0, 10)
    return ["C/" + (hit if isinstance(hit, str) else hit.path) for hit in matches]


def compare(args, name, case):
    language = name.split("-")[0]
    work = args.out / name
    html = work / "html"
    html.mkdir(parents=True)
    for path, title, body in case["docs"]:
        page = f'<!doctype html><html lang="{language}"><head><meta charset="utf-8"><title>{escape(title)}</title></head><body><p>{escape(body)}</p></body></html>'
        (html / (path + ".html")).write_text(page, encoding="utf-8")
    (html / "icon.png").write_bytes(
        base64.b64decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jrL8AAAAASUVORK5CYII="
        )
    )
    redirects = work / "redirects.tsv"
    redirects.write_text(
        f"alias.html\t{case['alias']}\t{case['docs'][1][0]}.html\n", encoding="utf-8"
    )
    archives = {}
    stats = {}
    problems = []
    snapshots_match = {}
    for label, builder in (
        ("native", args.native_builder),
        ("rust", args.rust_builder),
    ):
        zim = work / (label + ".zim")
        output(
            args.zimwriterfs,
            "--welcome=" + case["docs"][0][0] + ".html",
            "--illustration=icon.png",
            "--language=" + language,
            "--name=cjk",
            "--title=CJK",
            "--description=parity",
            "--creator=test",
            "--publisher=test",
            "--redirects",
            redirects,
            "--xapianbuilder-path",
            builder,
            html,
            zim,
        )
        archives[label] = Archive(zim)
        for kind in ("fulltext", "title"):
            db = work / f"{label}-{kind}.glass"
            db.write_bytes(
                output(args.zimdump, "show", "--ns=X", "--url=" + kind + "/xapian", zim)
            )
            output(args.check, db)
            lines = output(args.snapshot, "snapshot", db).decode("ascii").splitlines()
            stats[f"{label}-{kind}"] = dict(Counter(line.split()[0] for line in lines))
            # zimwriterfs also sends the redirect and the illustration to its title helper.
            expected_documents = len(case["docs"]) + (2 if kind == "title" else 0)
            if lines[0] != f"COUNT {expected_documents}":
                problems.append(
                    f"{label}-{kind} expected {expected_documents} documents, found {lines[0]}"
                )
            (work / f"{label}-{kind}.snapshot").write_text("\n".join(lines) + "\n")
    for kind in ("fulltext", "title"):
        left, right = [
            (work / f"{label}-{kind}.snapshot").read_text().splitlines(True)
            for label in ("native", "rust")
        ]
        snapshots_match[kind] = left == right
        if left != right:
            (work / (kind + ".diff")).write_text(
                "".join(difflib.unified_diff(left, right, "native", "rust"))
            )
            problems.append(f"{kind} index snapshot differs")
        if kind == "fulltext" and name == "zho":
            terms = {
                bytes.fromhex(line.split()[1]).decode()
                for line in left
                if line.startswith("TERM ")
            }
            for term in ("中国", "国语", "语言", "言研", "研究"):
                if term not in terms:
                    problems.append(f"native index missing regression bigram {term}")
    queries = []
    for mode in ("fulltext", "suggest"):
        for query in case[mode]:
            paths = {}
            for label, archive in archives.items():
                paths[label + "_libzim"] = native_paths(archive, mode, query)
                flags = ["--suggest"] if mode == "suggest" else []
                raw = output(
                    args.zimsearch,
                    *flags,
                    "--limit",
                    "10",
                    work / (label + ".zim"),
                    query,
                )
                paths[label + "_zimsearch"] = [
                    json.loads(line)["path"] for line in raw.splitlines()
                ]
            oracle = paths["native_libzim"]
            if any(value != oracle for value in paths.values()):
                problems.append(f"{mode} {query!r}: ordered paths differ")
            if query == case["multi"] and len(oracle) < 2:
                problems.append(f"{query!r}: expected multiple native hits")
            if query == case[mode][-1] and oracle:
                problems.append(f"{query!r}: expected a miss")
            queries.append({"mode": mode, "query": query, "paths": paths})
    return {
        "snapshots_match": snapshots_match,
        "stats": stats,
        "queries": queries,
        "problems": problems,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in (
        "zimwriterfs",
        "zimdump",
        "native-builder",
        "rust-builder",
        "zimsearch",
        "snapshot",
    ):
        parser.add_argument("--" + arg, required=True)
    parser.add_argument(
        "--case",
        choices=CASES,
        action="append",
        help="Run a selected corpus (repeatable; defaults to all four)",
    )
    parser.add_argument("--check", default="xapian-check")
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    if any(args.out.iterdir()):
        parser.error("--out must be empty")
    report = {name: compare(args, name, CASES[name]) for name in (args.case or CASES)}
    (args.out / "results.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2) + "\n"
    )
    for lang, result in report.items():
        print(lang, len(result["queries"]), "queries;", result["problems"] or "PASS")
    return int(any(result["problems"] for result in report.values()))


if __name__ == "__main__":
    raise SystemExit(main())
