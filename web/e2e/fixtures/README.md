# End-to-end test fixtures

`paper.zip` is the `paper/` folder zipped, the way an Overleaf or Prism export looks: the files
at the top level, `main.tex` holding `\documentclass`. `sections/intro.tex` has a typo
("descibes") on purpose: the fake model in `../fake-model.ts` proposes the fix.

The project only uses packages of Ubuntu's `texlive-latex-base`, which CI installs. After
changing `paper/`, rebuild the archive:

```sh
cd paper && rm -f ../paper.zip && zip -X -r ../paper.zip main.tex sections
```
