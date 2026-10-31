# The PyTorch CUDA images have Python 3.10, and `pyproject.toml` needs 3.12. The `torch` wheel of PyPI brings CUDA 12.
FROM python:3.12.14-slim
WORKDIR /train
COPY pyproject.toml train.py ./
RUN pip install --no-cache-dir .
CMD ["python", "train.py"]
