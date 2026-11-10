#!/bin/sh
# Creates the playground for the korin tutorial, in the current directory:
#   work/        a small git repository, with one release tagged v0.1.0
#   remote.git/  a bare repository that work/ pushes to, as `origin`
# Nothing here touches your other repositories or any server.
set -eu

git init --quiet --bare --initial-branch=main remote.git
git init --quiet --initial-branch=main work
cd work
git config user.name >/dev/null || git config user.name "Tutorial"
git config user.email >/dev/null || git config user.email "tutorial@example.com"
git remote add origin ../remote.git

printf '# shop\n\nA small online shop.\n' >README.md
# korin writes a journal of each run to .korin/; keep it out of git.
printf '.korin/\n' >.gitignore
git add README.md .gitignore
git commit --quiet --message "feat: start the shop"
git tag --annotate v0.1.0 --message "Release v0.1.0"

printf 'items = []\n' >cart.txt
git add cart.txt
git commit --quiet --message "feat: add a cart"
printf 'items = []\ntotal = 0\n' >cart.txt
git commit --quiet --all --message "fix: show the cart total"

git push --quiet origin main v0.1.0
echo "Ready: cd work, then run korin from there."
