# End-to-end test fixtures

`paper.zip` is the `paper/` folder zipped, the way an Overleaf or Prism export looks: the files
at the top level, `main.tex` holding `\documentclass`. `sections/intro.tex` has a typo
("descibes") on purpose: the fake model in `../fake-model.ts` proposes the fix.

The project only uses packages of Ubuntu's `texlive-latex-base`, which CI installs. After
changing `paper/`, rebuild the archive:

```sh
cd paper && rm -f ../paper.zip && zip -X -r ../paper.zip main.tex sections
```

`two-pages.pdf` is the PDF `../pdf-import.spec.ts` imports, made from `two-pages/main.tex`:
text on both pages and a grey box standing in for a drawing on page 1. Its fonts carry
ToUnicode maps (`\pdfgentounicode=1`), so the text layer reads back exactly; the fake model
copies a page from that text. Rebuild it with the TeX Live image:

```sh
cd two-pages
docker run --rm --network none --user "$(id -u):$(id -g)" -e HOME=/tmp \
  -v "$PWD:/work" -w /work texlive/texlive:latest \
  sh -c 'pdflatex -interaction=nonstopmode main.tex && rm -f main.aux main.log'
mv main.pdf ../two-pages.pdf
```
