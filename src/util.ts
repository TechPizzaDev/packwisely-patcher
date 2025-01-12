
export function toReadableSize(size: number, maxIndex = 9, si = true, delimeter = ""): string {
    const thresh = si ? 1000 : 1024;
    let log2 = Math.log2(size | 1);
    let index = Math.floor(log2 / Math.log2(thresh));
    let clamped = Math.min(index, maxIndex);
    return toFixedSize(size, clamped, si, delimeter);
}

export function toFixedSize(size: number, index: number, si = true, delimeter = ""): string {
    const thresh = si ? 1000 : 1024;
    const units = si
        ? ['B', 'kB', 'MB', 'GB', 'TB', 'PB', 'EB', 'ZB', 'YB']
        : ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB', 'EiB', 'ZiB', 'YiB'];

    let value = size / Math.pow(thresh, index);
    let prefix = value.toFixed(0);
    return prefix + delimeter + units[Math.min(index, units.length - 1)];
}