
let tabBodies = <HTMLCollectionOf<HTMLElement>>document.getElementsByClassName("tab-body");
let tabLinks = <HTMLCollectionOf<HTMLButtonElement>>document.getElementsByClassName("tab-link");

for (let link of tabLinks) {
    link.onclick = (evt) => {
        openTabByName(getTabName(<HTMLButtonElement>evt.currentTarget));
    };
}

tabLinks[0].click();

function openTabByName(name?: string) {
    for (let body of tabBodies) {
        if (getTabName(body) == name) {
            body.classList.add("visible");
        } else {
            body.classList.remove("visible");
        }
    }

    for (let link of tabLinks) {
        if (getTabName(link) == name) {
            link.classList.add("active");
        } else {
            link.classList.remove("active");
        }
    }
}

function getTabName(elem: HTMLElement): string | undefined {
    return elem.attributes.getNamedItem("x:tab")?.value;
}