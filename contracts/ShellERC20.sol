// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/**
 * @title ShellERC20
 * @notice Reference PQ-AA-compatible ERC-20 token for Shell Chain.
 *
 * Key differences from standard ERC-20:
 *  - No constructor msg.sender assumption; owner set via _initialize().
 *  - Emits AccountManager-compatible events for on-chain AA tracing.
 *  - Exposes the standard ERC-20 address-typed transfer interface.
 */
contract ShellERC20 {
    string public name;
    string public symbol;
    uint8 public decimals;
    uint256 public totalSupply;

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    address public owner;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    error InsufficientBalance(address account, uint256 balance, uint256 needed);
    error InsufficientAllowance(address spender, uint256 allowance, uint256 needed);
    error Unauthorized();
    error ZeroAddress();

    modifier onlyOwner() {
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    /**
     * @dev Initialize the token.  Call this once from your deployer contract
     *      (or constructor) rather than relying on constructor msg.sender so
     *      that PQ-AA accounts can own the contract from the start.
     */
    function _initialize(string memory _name, string memory _symbol, uint8 _decimals, address _owner) internal {
        name = _name;
        symbol = _symbol;
        decimals = _decimals;
        owner = _owner;
    }

    constructor(string memory _name, string memory _symbol, uint8 _decimals) {
        _initialize(_name, _symbol, _decimals, msg.sender);
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        _approve(msg.sender, spender, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 currentAllowance = allowance[from][msg.sender];
        if (currentAllowance != type(uint256).max) {
            if (currentAllowance < amount) revert InsufficientAllowance(msg.sender, currentAllowance, amount);
            unchecked { allowance[from][msg.sender] = currentAllowance - amount; }
        }
        _transfer(from, to, amount);
        return true;
    }

    function mint(address to, uint256 amount) external onlyOwner {
        if (to == address(0)) revert ZeroAddress();
        totalSupply += amount;
        unchecked { balanceOf[to] += amount; }
        emit Transfer(address(0), to, amount);
    }

    function burn(uint256 amount) external {
        _burn(msg.sender, amount);
    }

    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert ZeroAddress();
        emit OwnershipTransferred(owner, newOwner);
        owner = newOwner;
    }

    function _transfer(address from, address to, uint256 amount) internal {
        if (to == address(0)) revert ZeroAddress();
        uint256 fromBalance = balanceOf[from];
        if (fromBalance < amount) revert InsufficientBalance(from, fromBalance, amount);
        unchecked {
            balanceOf[from] = fromBalance - amount;
            balanceOf[to] += amount;
        }
        emit Transfer(from, to, amount);
    }

    function _approve(address _owner, address spender, uint256 amount) internal {
        if (_owner == address(0) || spender == address(0)) revert ZeroAddress();
        allowance[_owner][spender] = amount;
        emit Approval(_owner, spender, amount);
    }

    function _burn(address account, uint256 amount) internal {
        uint256 accountBalance = balanceOf[account];
        if (accountBalance < amount) revert InsufficientBalance(account, accountBalance, amount);
        unchecked {
            balanceOf[account] = accountBalance - amount;
            totalSupply -= amount;
        }
        emit Transfer(account, address(0), amount);
    }
}