// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/**
 * @dev Interface for the ERC-721 token receiver hook required by `safeTransferFrom`.
 *      Contracts that wish to receive ERC-721 tokens must implement this interface.
 */
interface IERC721Receiver {
    function onERC721Received(
        address operator,
        address from,
        uint256 tokenId,
        bytes calldata data
    ) external returns (bytes4);
}

/**
 * @title ShellERC721
 * @notice Reference PQ-AA-compatible ERC-721 NFT for Shell Chain.
 *
 * Follows EIP-721 (ERC-721 Non-Fungible Token Standard).
 * Owner initialization matches ShellERC20 pattern for PQ-AA compatibility.
 */
contract ShellERC721 {
    string public name;
    string public symbol;

    mapping(uint256 => address) private _ownerOf;
    mapping(address => uint256) private _balanceOf;
    mapping(uint256 => address) private _tokenApprovals;
    mapping(address => mapping(address => bool)) private _operatorApprovals;
    mapping(uint256 => string) private _tokenURIs;

    address public contractOwner;
    uint256 private _nextTokenId;

    event Transfer(address indexed from, address indexed to, uint256 indexed tokenId);
    event Approval(address indexed owner, address indexed approved, uint256 indexed tokenId);
    event ApprovalForAll(address indexed owner, address indexed operator, bool approved);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    error NotTokenOwnerOrApproved();
    error TokenNotFound(uint256 tokenId);
    error TokenAlreadyExists(uint256 tokenId);
    error ZeroAddress();
    error Unauthorized();
    error ERC721ReceiverCheckFailed(address to);

    modifier onlyContractOwner() {
        if (msg.sender != contractOwner) revert Unauthorized();
        _;
    }

    constructor(string memory _name, string memory _symbol) {
        name = _name;
        symbol = _symbol;
        contractOwner = msg.sender;
    }

    // ─── ERC-721 view functions ─────────────────────────────────────────────

    function balanceOf(address _owner) external view returns (uint256) {
        if (_owner == address(0)) revert ZeroAddress();
        return _balanceOf[_owner];
    }

    function ownerOf(uint256 tokenId) external view returns (address) {
        address tokenOwner = _ownerOf[tokenId];
        if (tokenOwner == address(0)) revert TokenNotFound(tokenId);
        return tokenOwner;
    }

    function tokenURI(uint256 tokenId) external view returns (string memory) {
        if (_ownerOf[tokenId] == address(0)) revert TokenNotFound(tokenId);
        return _tokenURIs[tokenId];
    }

    function getApproved(uint256 tokenId) external view returns (address) {
        if (_ownerOf[tokenId] == address(0)) revert TokenNotFound(tokenId);
        return _tokenApprovals[tokenId];
    }

    function isApprovedForAll(address _owner, address operator) external view returns (bool) {
        return _operatorApprovals[_owner][operator];
    }

    // ─── ERC-721 mutating functions ─────────────────────────────────────────

    function approve(address to, uint256 tokenId) external {
        address tokenOwner = _ownerOf[tokenId];
        if (tokenOwner == address(0)) revert TokenNotFound(tokenId);
        if (msg.sender != tokenOwner && !_operatorApprovals[tokenOwner][msg.sender]) {
            revert NotTokenOwnerOrApproved();
        }
        _tokenApprovals[tokenId] = to;
        emit Approval(tokenOwner, to, tokenId);
    }

    function setApprovalForAll(address operator, bool approved) external {
        if (operator == address(0)) revert ZeroAddress();
        _operatorApprovals[msg.sender][operator] = approved;
        emit ApprovalForAll(msg.sender, operator, approved);
    }

    function transferFrom(address from, address to, uint256 tokenId) external {
        if (to == address(0)) revert ZeroAddress();
        address tokenOwner = _ownerOf[tokenId];
        if (tokenOwner == address(0)) revert TokenNotFound(tokenId);
        if (
            msg.sender != tokenOwner &&
            msg.sender != _tokenApprovals[tokenId] &&
            !_operatorApprovals[tokenOwner][msg.sender]
        ) revert NotTokenOwnerOrApproved();
        if (from != tokenOwner) revert NotTokenOwnerOrApproved();
        _transfer(from, to, tokenId);
    }

    function safeTransferFrom(address from, address to, uint256 tokenId) external {
        safeTransferFrom(from, to, tokenId, "");
    }

    function safeTransferFrom(address from, address to, uint256 tokenId, bytes calldata data) external {
        transferFrom(from, to, tokenId);
        _checkOnERC721Received(msg.sender, from, to, tokenId, data);
    }

    /// @dev Calls `onERC721Received` on `to` if it is a contract, and reverts
    ///      if the return value is not the expected magic selector.
    function _checkOnERC721Received(
        address operator,
        address from,
        address to,
        uint256 tokenId,
        bytes memory data
    ) private {
        if (to.code.length > 0) {
            try IERC721Receiver(to).onERC721Received(operator, from, tokenId, data) returns (bytes4 retval) {
                if (retval != IERC721Receiver.onERC721Received.selector) {
                    revert ERC721ReceiverCheckFailed(to);
                }
            } catch {
                revert ERC721ReceiverCheckFailed(to);
            }
        }
    }

    // ─── Minting / burning (onlyContractOwner) ──────────────────────────────

    function mint(address to, string calldata uri) external onlyContractOwner returns (uint256) {
        if (to == address(0)) revert ZeroAddress();
        uint256 tokenId = _nextTokenId++;
        _ownerOf[tokenId] = to;
        unchecked { _balanceOf[to]++; }
        _tokenURIs[tokenId] = uri;
        emit Transfer(address(0), to, tokenId);
        return tokenId;
    }

    function burn(uint256 tokenId) external {
        address tokenOwner = _ownerOf[tokenId];
        if (tokenOwner == address(0)) revert TokenNotFound(tokenId);
        if (msg.sender != tokenOwner && !_operatorApprovals[tokenOwner][msg.sender]) {
            revert NotTokenOwnerOrApproved();
        }
        _transfer(tokenOwner, address(0), tokenId);
        delete _tokenURIs[tokenId];
    }

    function transferOwnership(address newOwner) external onlyContractOwner {
        if (newOwner == address(0)) revert ZeroAddress();
        emit OwnershipTransferred(contractOwner, newOwner);
        contractOwner = newOwner;
    }

    // ─── Internal ────────────────────────────────────────────────────────────

    function _transfer(address from, address to, uint256 tokenId) internal {
        delete _tokenApprovals[tokenId];
        _ownerOf[tokenId] = to;
        if (from != address(0)) unchecked { _balanceOf[from]--; }
        if (to != address(0)) unchecked { _balanceOf[to]++; }
        emit Transfer(from, to, tokenId);
    }

    // ─── ERC-165 ─────────────────────────────────────────────────────────────

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return
            interfaceId == 0x80ac58cd || // ERC-721
            interfaceId == 0x5b5e139f || // ERC-721 Metadata
            interfaceId == 0x01ffc9a7;   // ERC-165
    }
}