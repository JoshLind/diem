The Libra Developer Docs website was created with [Docusaurus](https://docusaurus.io/).
FontAwesome icons were used under the
[Creative Commons Attribution 4.0 International](https://fontawesome.com/license).

## Building

You need [Node](https://nodejs.org/en/) >= 8.x and
[Yarn](https://yarnpkg.com/en/) >= 1.5 in order to build the libra website.

Switch to the `website` dir from the project root and start the server:
```bash
cd website
yarn start
```

Open http://localhost:3000 (if doesn't automatically open).

Anytime you change the contents of the page, the page should auto-update.

Note that the above does not re-build the API reference (auto-generated by
Rustdoc and Protogen). To generate these pages, you can run the following:
```bash
./scripts/build_docs.sh
```

#### Generating a static build

To generate a static build of the website in the `website/build` directory, run
```bash
./scripts/build_docs.sh -b
```

#### Deploying for wider testing

```bash
zip libra.zip -r website/build
scp -r website/build/ user@server:/path
```

on server:
```bash
unzip libra.zip
```

## Publishing

The site is hosted on GitHub pages, via the `gh-pages` branch of the `website'
[GitHub repo](https://github.com/libra/website).

The website is automatically built and published from CircleCI - see the
[config file](https://github.com/libra/website/blob/master/.circleci/config.yml)
for details.