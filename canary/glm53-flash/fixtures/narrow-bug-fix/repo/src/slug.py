import re


def slugify(text):
    """Return a URL slug."""
    return "-".join(text.lower().split())
