# Remove Python bytecode caches (__pycache__, *.pyc/*.pyo) under the current tree. Portable.
pyclean() {
  find . -type d -name __pycache__ -prune -exec rm -rf {} + 2>/dev/null
  find . -type f \( -name '*.pyc' -o -name '*.pyo' \) -delete 2>/dev/null
  echo "pyclean: removed __pycache__ and compiled files"
}
