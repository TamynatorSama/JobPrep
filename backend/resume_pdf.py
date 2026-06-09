"""Render the tailored-resume plain text into an ATS-friendly PDF.

Mirrors the layout rules of routes.application._build_docx so the PDF and DOCX
look the same:

    line 1        -> name (centered, 20pt bold)
    line 2        -> contact (centered, 10pt grey)
    '## SECTION'  -> uppercase heading + bottom rule
    '### Sub'     -> bold subhead (role / project / school)
    '- bullet'    -> bullet with hanging indent
    '**text**'    -> inline bold

Uses the PDF base-14 Times fonts (no embedding needed). reportlab handles word
wrapping, which is why we render from the structured text rather than converting
the .docx (that would need Word/LibreOffice installed).
"""
from __future__ import annotations

import io
from xml.sax.saxutils import escape

from reportlab.lib.colors import HexColor
from reportlab.lib.enums import TA_CENTER
from reportlab.lib.pagesizes import LETTER
from reportlab.lib.styles import ParagraphStyle
from reportlab.lib.units import inch
from reportlab.platypus import HRFlowable, Paragraph, SimpleDocTemplate, Spacer

_GREY = HexColor("#555555")
_INK = HexColor("#101010")
_RULE = HexColor("#606060")


def _inline(text: str) -> str:
    """XML-escape, then turn **bold** spans into <b> for reportlab markup."""
    parts = escape(text).split("**")
    out = []
    for i, seg in enumerate(parts):
        if not seg:
            continue
        out.append(f"<b>{seg}</b>" if i % 2 == 1 else seg)
    return "".join(out)


def candidate_name(resume_text: str) -> str:
    """First non-empty line of the resume is the candidate's name (see the
    _build_docx layout contract)."""
    for line in resume_text.split("\n"):
        if line.strip():
            return line.strip()
    return ""


def build_pdf(resume_text: str) -> bytes:
    buf = io.BytesIO()
    doc = SimpleDocTemplate(
        buf, pagesize=LETTER,
        topMargin=0.5 * inch, bottomMargin=0.5 * inch,
        leftMargin=0.6 * inch, rightMargin=0.6 * inch,
        title="Resume",
    )

    name_s = ParagraphStyle("name", fontName="Times-Bold", fontSize=20, leading=23,
                            alignment=TA_CENTER, textColor=_INK, spaceAfter=0)
    contact_s = ParagraphStyle("contact", fontName="Times-Roman", fontSize=10, leading=12,
                               alignment=TA_CENTER, textColor=_GREY, spaceAfter=6)
    sec_s = ParagraphStyle("sec", fontName="Times-Bold", fontSize=11.5, leading=14,
                           textColor=_INK, spaceBefore=8, spaceAfter=2)
    sub_s = ParagraphStyle("sub", fontName="Times-Bold", fontSize=11, leading=13,
                           spaceBefore=3, spaceAfter=1)
    body_s = ParagraphStyle("body", fontName="Times-Roman", fontSize=11, leading=13, spaceAfter=2)
    bullet_s = ParagraphStyle("bullet", parent=body_s, leftIndent=14, bulletIndent=4, spaceAfter=1)

    flow = []
    saw_name = saw_contact = False
    for raw in resume_text.split("\n"):
        line = raw.strip()
        if not line:
            flow.append(Spacer(1, 3))
            continue
        if not saw_name:
            flow.append(Paragraph(_inline(line), name_s)); saw_name = True; continue
        if not saw_contact:
            flow.append(Paragraph(_inline(line), contact_s)); saw_contact = True; continue
        if line.startswith("## "):
            flow.append(Paragraph(_inline(line[3:].strip().upper()), sec_s))
            flow.append(HRFlowable(width="100%", thickness=0.7, color=_RULE, spaceBefore=1, spaceAfter=3))
            continue
        if line.startswith("# "):
            flow.append(Paragraph(_inline(line[2:].strip()), sec_s)); continue
        if line.startswith("### "):
            flow.append(Paragraph(_inline(line[4:].strip()), sub_s)); continue
        if line.startswith("- ") or line.startswith("* "):
            flow.append(Paragraph(_inline(line[2:].strip()), bullet_s, bulletText="•")); continue
        # Plain paragraph (handles bare "**Heading**" via _inline too).
        flow.append(Paragraph(_inline(line), body_s))

    doc.build(flow)
    return buf.getvalue()
