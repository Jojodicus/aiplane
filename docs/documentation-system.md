# Documentation and LLM downloads

Open **Documentation** in the application navigation or on the sign-in page.
The manual is available before initial setup and does not require login. It is
packaged with AIplane, so its version and commit identify the application build
you are using.

The search field finds product guides and references. Search, styles and images
are served by your installation and remain available without an internet
connection. Links to external services and repository pages need internet
access.

## Read the manual with an LLM

The documentation is available as Markdown alongside the website:

| Installed URL | Contents |
|---|---|
| `/docs/llms.txt` | Topic index with links to each published Markdown page |
| `/docs/llms-full.txt` | All published pages combined into one text file |
| `/docs/markdown/<path>.md` | One page, preserving its headings, examples and links |
| `/docs/build-info.json` | Version, commit and build state |

An LLM client can read the index, follow links to the pages it needs, or load
the full text file. Images are separate files; their alt text stays in Markdown.
The text export is a download: AIplane does not automatically include the manual
in a model request.

The downloaded manual and website use the same content and links. Repository
references point to the code revision used to build your installation. The
public documentation site uses the same manual when enabled for the repository.
